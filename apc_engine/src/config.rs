// src/config.rs
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;
use anyhow::{ensure, Context, Result};

// --- ROOT STRUCTURE ---
#[derive(Debug, Deserialize, Clone)]
pub struct UnifiedModel {
    pub metadata: Metadata,
    pub tuning: Tuning,
    pub variables: Variables,
    pub physics: Physics,
    #[serde(default)]
    pub control_strategy: ControlStrategy,
}

impl UnifiedModel {
    pub fn load(path: &str) -> Result<Self> {
        Self::load_verified(path, None).map(|(model, _)| model)
    }

    /// Hash and parse the same owned byte buffer; return its actual digest even
    /// for manual launches, and never reopen a verified path.
    pub fn load_verified(path: &str, expected_sha256: Option<&str>) -> Result<(Self, String)> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("Failed to read config file: {path}"))?;
        Self::from_verified_bytes(&bytes, expected_sha256)
    }

    fn from_verified_bytes(bytes: &[u8], expected_sha256: Option<&str>) -> Result<(Self, String)> {
        let actual = format!("{:x}", Sha256::digest(bytes));
        if let Some(expected) = expected_sha256 {
            ensure!(expected.len() == 64 && expected.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "expected-model-sha256 must contain exactly 64 lowercase hexadecimal characters");
            ensure!(actual == expected, "Model SHA-256 mismatch: expected {expected}, read {actual}");
        }
        let config: Self = serde_json::from_slice(bytes).context("Failed to parse JSON config")?;
        config.validate_runtime()?;
        Ok((config, actual))
    }

    pub fn sample_interval(&self) -> Result<Duration> {
        positive_duration(self.tuning.sample_time, "sample_time")
    }

    /// Validate before coefficient generation, indexing or duration conversion.
    pub fn validate_runtime(&self) -> Result<()> {
        self.sample_interval()?;
        self.validate_control_strategy()?;
        let t = &self.tuning;
        ensure!(t.prediction_horizon > 0 && t.control_horizon > 0 && t.control_horizon <= t.prediction_horizon,
            "Invalid prediction/control horizons");
        ensure!(t.solver_tolerance.is_finite() && t.solver_tolerance > 0.0 && t.max_iterations > 0,
            "Invalid solver tolerance/iteration limit");
        ensure!(t.terminal_weight_factor.is_finite() && t.terminal_weight_factor > 0.0,
            "Invalid terminal weight");
        let nc = self.variables.cvs.len();
        let nm = self.variables.mvs.len();
        ensure!(nc > 0 && nm > 0, "At least one CV and MV are required");
        for cv in &self.variables.cvs {
            ensure!(ordered_limits(cv.limits.low_low, cv.limits.low, cv.limits.high, cv.limits.high_high),
                "Invalid CV limits: {}", cv.name);
            ensure!(cv.limits.target.is_finite() && cv.limits.target >= cv.limits.low && cv.limits.target <= cv.limits.high,
                "Invalid CV target: {}", cv.name);
            ensure!(cv.weight.is_finite() && cv.weight >= 0.0 && cv.alpha.is_finite() && (0.0..=1.0).contains(&cv.alpha)
                && cv.ece_factor.is_finite() && cv.ece_factor > 0.0 && cv.slack_weight.is_finite() && cv.slack_weight >= 0.0,
                "Invalid CV tuning: {}", cv.name);
            if let OptimizationMode::Target { value } = cv.optimization_mode {
                ensure!(value.is_finite(), "Nonfinite CV optimization target");
            }
        }
        for mv in &self.variables.mvs {
            ensure!(ordered_limits(mv.limits.low_low, mv.limits.low, mv.limits.high, mv.limits.high_high),
                "Invalid MV limits: {}", mv.name);
            ensure!(mv.max_move.is_finite() && mv.max_move > 0.0 && mv.weight_r.is_finite() && mv.weight_r >= 0.0
                && mv.target_weight.is_finite() && mv.target_weight >= 0.0 && mv.target.is_none_or(f64::is_finite),
                "Invalid MV tuning: {}", mv.name);
            ensure!(mv.target_weight == 0.0 || mv.node_ids.target.is_some() || mv.target.is_some(),
                "Weighted MV target needs a configured target or readable target node: {}", mv.name);
            if let MvOptimizationMode::Target { value } = mv.optimization_mode {
                ensure!(value.is_finite(), "Nonfinite MV optimization target");
            }
        }
        for dv in &self.variables.dvs {
            ensure!(dv.limits.low.is_finite() && dv.limits.high.is_finite() && dv.limits.low < dv.limits.high,
                "Invalid DV limits: {}", dv.name);
        }
        if self.metadata.model_type == "parametric" {
            for matrix in [&self.physics.gain, &self.physics.tau, &self.physics.dead_time] {
                ensure!(matrix.len() == nc && matrix.iter().all(|r| r.len() == nm && r.iter().all(|x| x.is_finite())),
                    "Invalid parametric MV matrix dimensions/values");
            }
            ensure!(self.physics.tau.iter().flatten().all(|v| *v >= 0.0)
                && self.physics.dead_time.iter().flatten().all(|v| *v >= 0.0), "Negative time constant/dead time");
        }
        Ok(())
    }

    pub fn validate_live_tuning(&self, cv_weights: &[f64], cv_alphas: &[f64], mv_weights: &[f64], frozen: bool) -> Result<()> {
        ensure!(cv_weights.len() == self.variables.cvs.len() && cv_alphas.len() == self.variables.cvs.len()
            && mv_weights.len() == self.variables.mvs.len(), "Incomplete live tuning vectors");
        ensure!(cv_weights.iter().chain(mv_weights).all(|v| v.is_finite() && *v >= 0.0)
            && cv_alphas.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)), "Invalid live tuning");
        if frozen {
            ensure!(self.variables.cvs.iter().enumerate().all(|(i,cv)| cv_weights[i] == cv.weight && cv_alphas[i] == cv.alpha)
                && self.variables.mvs.iter().enumerate().all(|(i,mv)| mv_weights[i] == mv.weight_r),
                "Live tuning differs from the SHA-256 selected model; restore selected values or select a new version");
        }
        Ok(())
    }

    /// The method is fixed for the controller lifetime; live weights/targets remain supported.
    pub fn validate_control_strategy(&self) -> Result<()> {
        let strategy = &self.control_strategy;
        ensure!(strategy.version == 1, "Unsupported control_strategy version: {}", strategy.version);
        match strategy.method {
            ControlMethod::StandardDmc => {
                ensure!(strategy.bands.is_empty(), "standard_dmc requires empty control_strategy bands");
            }
            ControlMethod::SoftBand => {
                ensure!(!self.variables.cvs.is_empty() && strategy.bands.len() == self.variables.cvs.len(),
                    "soft_band requires exactly one band per CV");
                for (i, (cv, band)) in self.variables.cvs.iter().zip(&strategy.bands).enumerate() {
                    ensure!(matches!(cv.optimization_mode, OptimizationMode::Target { .. }),
                        "soft_band CV {i} must use Target optimization, not Zone/Minimize/Maximize");
                    ensure!(band.half_width.is_finite() && band.half_width > 0.0,
                        "soft_band CV {i} half_width must be finite and positive");
                    ensure!(band.tracking_scale.is_finite() && band.tracking_scale > 0.0 && band.tracking_scale <= 1.0,
                        "soft_band CV {i} tracking_scale must be in (0, 1]");
                    ensure!(band.quadratic_penalty.is_finite() && band.quadratic_penalty > 0.0,
                        "soft_band CV {i} quadratic_penalty must be finite and positive");
                    ensure!(cv.ece_factor.is_finite() && cv.ece_factor > 0.0,
                        "soft_band CV {i} ece_factor must be finite and positive");
                }
                for (i, mv) in self.variables.mvs.iter().enumerate() {
                    ensure!(matches!(mv.optimization_mode, MvOptimizationMode::Target { .. }),
                        "soft_band MV {i} must use Target optimization, not economic Minimize/Maximize");
                }
            }
        }
        Ok(())
    }
}

pub fn positive_duration(seconds: f64, name: &str) -> Result<Duration> {
    ensure!(seconds.is_finite() && seconds > 0.0, "{name} must be finite and positive");
    let value = Duration::try_from_secs_f64(seconds).with_context(|| format!("{name} is not a representable duration"))?;
    ensure!(!value.is_zero(), "{name} must be at least one nanosecond");
    // Tokio deadlines must also be representable, not merely Duration itself.
    ensure!(std::time::Instant::now().checked_add(value).is_some(), "{name} exceeds clock range");
    Ok(value)
}

pub fn ordered_limits(ll: f64, low: f64, high: f64, hh: f64) -> bool {
    [ll, low, high, hh].iter().all(|v| v.is_finite()) && ll <= low && low < high && high <= hh
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ControlMethod {
    #[default]
    StandardDmc,
    SoftBand,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct QualityBand {
    /// Engineering-unit half width about the live target, not the operating limits.
    pub half_width: f64,
    pub tracking_scale: f64,
    pub quadratic_penalty: f64,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ControlStrategy {
    pub method: ControlMethod,
    pub version: u32,
    pub bands: Vec<QualityBand>,
}

impl Default for ControlStrategy {
    fn default() -> Self {
        Self { method: ControlMethod::StandardDmc, version: 1, bands: Vec::new() }
    }
}

// --- SUB-STRUCTURES ---

#[derive(Debug, Deserialize, Clone)]
pub struct Metadata {
    pub name: String,
    pub description: String,
    pub version: String,
    #[serde(default = "default_model_type")]
    pub model_type: String, // "parametric" or "step_response"
}

fn default_model_type() -> String {
    "parametric".to_string()
}

#[derive(Debug, Deserialize, Clone)]
pub struct Tuning {
    pub prediction_horizon: usize,
    pub control_horizon: usize,
    pub sample_time: f64,
    pub solver_tolerance: f64,
    pub max_iterations: usize,
    #[serde(default = "default_terminal_weight")]
    pub terminal_weight_factor: f64, // Feature 3: Multiply last step weight (default 10.0)
}

fn default_terminal_weight() -> f64 {
    10.0 // Industry standard: heavily weight terminal state for stability
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct Variables {
    pub cvs: Vec<CvConfig>,
    pub mvs: Vec<MvConfig>,
    #[serde(default)]
    pub dvs: Vec<DvConfig>,
}

// --- CV DEFINITIONS ---

/// Optimization mode for CVs
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(tag = "type")]
pub enum OptimizationMode {
    /// Traditional target tracking: minimize (CV - target)^2
    Target { 
        #[allow(dead_code)] // Used in pattern matching in DMC logic
        value: f64 
    },
    /// Zone control: no cost if within [low, high], penalize violations
    Zone,
    /// Maximize CV within limits (linear cost pushing toward high limit)
    Maximize,
    /// Minimize CV within limits (linear cost pushing toward low limit)
    Minimize,
}

impl Default for OptimizationMode {
    fn default() -> Self {
        OptimizationMode::Target { value: 0.0 }
    }
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct CvConfig {
    pub name: String,
    pub description: String,
    pub units: String,
    pub weight: f64,
    #[serde(default)] // Default to 0.0 if missing (Backwards Compat)
    pub alpha: f64,
    #[serde(default = "default_ece_factor")] // Default to 1.0 if missing
    pub ece_factor: f64, // Equal Concern Error normalization factor
    #[serde(default)] // Default to Target mode with value from limits.target
    pub optimization_mode: OptimizationMode,
    #[serde(default = "default_slack_weight")] // Penalty for soft constraint violations
    pub slack_weight: f64,
    #[serde(default)] // Feature 2: Integrating variable flag
    pub is_integrating: bool, // If true, use input disturbance instead of output bias
    pub limits: CvLimits,
    pub node_ids: CvNodes,
}

fn default_ece_factor() -> f64 {
    1.0
}

fn default_slack_weight() -> f64 {
    1000.0 // High penalty for constraint violations
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct CvLimits {
    pub low_low: f64,
    pub low: f64,
    pub target: f64,
    pub high: f64,
    pub high_high: f64,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct CvNodes {
    pub pv: String,
    pub target: String,
    pub prediction: String,
    // Included to match model.json structure
    #[allow(dead_code)] // Reserved for future HMI/DCS integration
    pub limits: LimitNodes, 
}

// --- MV DEFINITIONS ---
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(tag = "type")]
pub enum MvOptimizationMode {
    /// Traditional MV target tracking
    Target {
        #[allow(dead_code)]
        value: f64,
    },
    /// Push MV toward upper operating limit
    Maximize,
    /// Push MV toward lower operating limit
    Minimize,
}

impl Default for MvOptimizationMode {
    fn default() -> Self {
        MvOptimizationMode::Target { value: 0.0 }
    }
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct MvConfig {
    pub name: String,
    pub description: String, //
    pub units: String,
    pub weight_r: f64,
    pub max_move: f64,
    #[serde(default)]
    pub optimization_mode: MvOptimizationMode,
    #[serde(default)]
    pub target: Option<f64>,        // Optional target for economic MVs
    #[serde(default)]
    pub target_weight: f64,         // Weight for target tracking (0.0 = no target)
    pub limits: MvLimits,
    pub node_ids: MvNodes,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct MvLimits {
    pub low_low: f64,
    pub low: f64,
    pub high: f64,
    pub high_high: f64,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct MvNodes {
    #[allow(dead_code)] // Reserved for future feedback validation
    pub pv: String,          // Actual process measurement
    pub sp: String,          // Setpoint (where MPC writes)
    /// Effective lower-PID setpoint feedback, in the same units as the commanded SP.
    /// When absent, the SP node is treated as one readable/writable setpoint coordinate.
    #[serde(default)]
    pub applied_sp: Option<String>,
    pub op: String,          // PID output to valve
    pub mode: String,        // Actual PID mode; numeric meanings come from the commissioned profile.
    pub mode_target: String, // Independent requested mode; never accepted as actual-mode feedback.
    #[serde(default)]
    pub target: Option<String>, // Optional economic target node
    pub future_plan: String,
    pub limits: LimitNodes, 
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct DvConfig {
    pub name: String,
    pub description: String,
    pub units: String,
    pub limits: DvLimits,
    pub node_ids: DvNodes,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct DvLimits {
    pub low: f64,
    pub high: f64,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct DvNodes {
    pub pv: String,
    pub limits: DvLimitNodes,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct DvLimitNodes {
    pub high: String,
    pub low: String,
}

// --- SHARED STRUCTS ---
#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct LimitNodes {
    pub high: String,
    pub low: String,
    pub hh: String,
    pub ll: String,
}

// --- PHYSICS (MATRICES) ---
#[derive(Debug, Deserialize, Clone)]
pub struct Physics {
    // Parametric model (FOPDT)
    #[serde(default)]
    pub gain: Vec<Vec<f64>>,
    #[serde(default)]
    pub tau: Vec<Vec<f64>>,
    #[serde(default)]
    pub dead_time: Vec<Vec<f64>>,
    
    // DV (Disturbance Variable) FOPDT parameters
    #[serde(default)]
    pub gain_dv: Vec<Vec<f64>>,
    #[serde(default)]
    pub tau_dv: Vec<Vec<f64>>,
    #[serde(default)]
    pub dead_time_dv: Vec<Vec<f64>>,
    
    // Non-parametric model (Step Response Coefficients)
    // step_coefficients[cv_idx][mv_idx][time_step]
    #[serde(default)]
    pub step_coefficients: Vec<Vec<Vec<f64>>>,
    
    // dv_coefficients[cv_idx][dv_idx][time_step]
    #[serde(default)]
    pub dv_coefficients: Vec<Vec<Vec<f64>>>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AppSettings {
    pub opcua: OpcuaConfig,
    pub identity: IdentityConfig,
    pub auth: AuthConfig,
    pub runtime: RuntimeConfig,
    pub paths: PathConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct OpcuaConfig {
    pub endpoint_url: String,
    pub security_policy: String,
    pub message_mode: String,
    pub namespace_index: u16,
}

#[derive(Debug, Deserialize, Clone)]
pub struct IdentityConfig {
    pub app_name: String,
    pub app_uri: String,
    pub auto_create_keys: bool,
    pub trust_server_certs: bool,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    Username,
    X509,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AuthConfig {
    pub mode: AuthMode,
    pub username: Option<String>,
    pub password: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RuntimeConfig {
    pub model_path: String,
    #[allow(dead_code)] // Reserved for future reconnection logic
    pub reconnect_delay_sec: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PathConfig {
    #[allow(dead_code)] // Reserved for alternative PKI configuration
    pub pki_dir: String,
    pub cert_path: String,
    pub key_path: String,
}

// Remove impl AppSettings::load as we will build it manually

#[cfg(test)]
mod safety_tests {
    use super::*;

    fn model_bytes() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "metadata": {"name":"safety","description":"","version":"1","model_type":"step_response"},
            "tuning": {"prediction_horizon":2,"control_horizon":1,"sample_time":1.0,
                "solver_tolerance":1e-6,"max_iterations":100},
            "variables": {
                "cvs": [{"name":"CV","description":"","units":"unit","weight":1.0,
                    "limits":{"low_low":0.0,"low":1.0,"target":5.0,"high":9.0,"high_high":10.0},
                    "node_ids":{"pv":"CV:PV","target":"CV:Target","prediction":"CV:Prediction",
                        "limits":{"low":"CV:L","high":"CV:H","ll":"CV:LL","hh":"CV:HH"}}}],
                "mvs": [{"name":"MV","description":"","units":"unit","weight_r":1.0,"max_move":1.0,
                    "limits":{"low_low":0.0,"low":1.0,"high":9.0,"high_high":10.0},
                    "node_ids":{"pv":"MV:PV","sp":"MV:SP","op":"MV:OP","mode":"MV:Mode",
                        "mode_target":"MV:ModeTarget","future_plan":"MV:Plan",
                        "limits":{"low":"MV:L","high":"MV:H","ll":"MV:LL","hh":"MV:HH"}}}]
            },
            "physics":{"step_coefficients":[[[0.2,0.5,1.0]]]}
        })).unwrap()
    }

    #[test]
    fn verification_is_bound_to_the_exact_parsed_buffer() {
        let bytes = model_bytes();
        let expected = format!("{:x}", Sha256::digest(&bytes));
        let (_,actual)=UnifiedModel::from_verified_bytes(&bytes, Some(&expected)).unwrap();
        assert_eq!(actual,expected);
        let mut changed = bytes.clone();
        changed.push(b' ');
        // Same JSON model, different bytes: cannot silently substitute it.
        let (_,changed_digest)=UnifiedModel::from_verified_bytes(&changed, None).unwrap();
        assert_eq!(changed_digest,format!("{:x}",Sha256::digest(&changed)));
        assert_ne!(actual,changed_digest);
        assert!(UnifiedModel::from_verified_bytes(&changed, Some(&expected)).is_err());
        assert!(UnifiedModel::from_verified_bytes(&bytes, Some(&expected.to_uppercase())).is_err());
        assert!(UnifiedModel::from_verified_bytes(&bytes, Some("abc")).is_err());
    }

    #[test]
    fn invalid_sampling_is_rejected_before_duration_or_controller_construction() {
        let (mut model, _) = UnifiedModel::from_verified_bytes(&model_bytes(), None).unwrap();
        for value in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::MAX, 1e-20] {
            model.tuning.sample_time = value;
            assert!(model.validate_runtime().is_err());
        }
        model.tuning.sample_time = 0.001;
        assert_eq!(model.sample_interval().unwrap(), Duration::from_millis(1));
    }

    #[test]
    fn static_tuning_limits_and_missing_weighted_targets_fail_closed() {
        let (base, _) = UnifiedModel::from_verified_bytes(&model_bytes(), None).unwrap();
        let mut model = base.clone();
        model.variables.cvs[0].ece_factor = 0.0;
        assert!(model.validate_runtime().is_err());
        let mut model = base.clone();
        model.variables.mvs[0].target_weight = 1.0;
        assert!(model.validate_runtime().is_err());
        model.variables.mvs[0].target = Some(5.0);
        assert!(model.validate_runtime().is_ok());
        let mut model = base.clone();
        model.variables.cvs[0].limits.high = 0.5;
        assert!(model.validate_runtime().is_err());
        let mut model = base;
        model.variables.cvs[0].alpha = 1.01;
        assert!(model.validate_runtime().is_err());
    }

    #[test]
    fn immutable_tuning_cannot_silently_become_live_tuning() {
        let (model, _) = UnifiedModel::from_verified_bytes(&model_bytes(), None).unwrap();
        assert!(model.validate_live_tuning(&[1.0], &[0.0], &[1.0], true).is_ok());
        assert!(model.validate_live_tuning(&[2.0], &[0.0], &[1.0], true).is_err());
        assert!(model.validate_live_tuning(&[1.0], &[0.5], &[1.0], true).is_err());
        assert!(model.validate_live_tuning(&[1.0], &[0.0], &[2.0], true).is_err());
        assert!(model.validate_live_tuning(&[2.0], &[0.5], &[2.0], false).is_ok());
        assert!(model.validate_live_tuning(&[], &[0.0], &[1.0], false).is_err());
        assert!(model.validate_live_tuning(&[f64::NAN], &[0.0], &[1.0], false).is_err());
        assert!(model.validate_live_tuning(&[1.0], &[1.01], &[1.0], false).is_err());
    }
}
