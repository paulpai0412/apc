mod config;
mod control_authority;
mod control_observation;
mod monitoring;
mod live_forecast;
mod dmc;
mod opc_interface;
mod modelloader;
mod native_lifecycle;
mod native_handoff;
mod native_approval;

use anyhow::{Result, anyhow, ensure, Context};
use clap::Parser;
use config::{UnifiedModel, AppSettings, AuthMode};
use control_authority::{AcquisitionEvidence, CommandLimits, Confirmation, ControlAction, ControlAuthority, HoldReason, PendingCommands, dispatch_contract_matches, validate_process_group};
use control_observation::RuntimeObservation;
use monitoring::RollingPerformance;
use dmc::DmcController;
use modelloader::{load_parametric_model, load_stepresponse_model};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use serde_json::json;

use opcua::{
    client::{ClientBuilder, IdentityToken, Session},
    crypto::SecurityPolicy,
    crypto::{X509, PrivateKey},
    types::{MessageSecurityMode, NodeId, UserTokenPolicy, UserTokenType},
};

struct ControlTelemetry<'a> {
    status: &'a NodeId,
    observation: &'a NodeId,
    forecast: &'a NodeId,
    engine_id: &'a str,
    native: &'a native_lifecycle::Native,
}

async fn observation_unavailable(session: &Session, telemetry: &ControlTelemetry<'_>, reason: &str) -> Result<()> {
    let payload = control_observation::unavailable_json(telemetry.engine_id, reason)?;
    let forecast = opc_interface::write_string(session, telemetry.forecast, "null").await;
    let observation = opc_interface::write_string(session, telemetry.observation, &payload).await;
    forecast?;
    observation
}

fn reset_observations(dmc: &mut DmcController, performance: &mut RollingPerformance) {
    dmc.reset_observation_history();
    performance.reset();
}

async fn hold_control(
    session: &Session,
    telemetry: &ControlTelemetry<'_>,
    authority: &mut ControlAuthority,
    reason: HoldReason,
    detail: impl std::fmt::Display,
) {
    authority.trip(reason);
    telemetry.native.fault(format!("{reason:?}: {detail}"));
    eprintln!("Control held ({reason:?}): {detail}");
    let message = format!("Hold ({reason:?}): {detail}; select Monitor, wait for readiness, then explicitly request Engage");
    match tokio::time::timeout(Duration::from_secs(2), async {
        let observation = observation_unavailable(session, telemetry, &message).await;
        let status = opc_interface::write_string(session, telemetry.status, &message).await;
        status?;
        observation
    }).await {
        Ok(Ok(())) => {}
        failure => eprintln!("Hold telemetry unconfirmed; local interlock stays latched: {failure:?}"),
    }
}

/// Read the producer's odd/even snapshot protocol. A publisher in progress is not
/// a sensor fault: retry torn frames inside the caller's mandatory time budget.
async fn read_acquisition_snapshot(
    session: &Session,
    sequence: &opc_interface::NumericReadPlan,
    signals: &opc_interface::NumericReadPlan,
    sequence_values: &mut Vec<f64>,
    values: &mut Vec<f64>,
) -> Result<f64> {
    ensure!(sequence.len() == 1, "Exactly one external acquisition sequence is required");
    loop {
        sequence.read_into(session, sequence_values).await?;
        let before = sequence_values[0];
        ensure!(before >= 0.0 && before <= (1_u64 << 53) as f64 && before.fract() == 0.0,
            "Invalid acquisition sequence");
        if (before as u64) % 2 != 0 {
            tokio::task::yield_now().await;
            continue;
        }
        signals.read_into(session, values).await?;
        sequence.read_into(session, sequence_values).await?;
        let after = sequence_values[0];
        ensure!(after >= 0.0 && after <= (1_u64 << 53) as f64 && after.fract() == 0.0,
            "Invalid acquisition sequence");
        if before == after {
            return Ok(after);
        }
        tokio::task::yield_now().await;
    }
}

fn normalize_modes(values: &[f64], profile: Option<&native_lifecycle::Commissioning>, output: &mut [f64]) {
    output.fill(0.0);
    if let Some(profile) = profile {
        for ((value, variable), normalized) in values.iter()
            .zip(profile.variables.iter().filter(|v| v.role == "MV")).zip(output) {
            *normalized = if !value.is_finite() { f64::NAN }
                else if Some(*value) == variable.remote_mode { 3.0 }
                else if *value == profile.handoff.local_mode { 0.0 } else { f64::NAN };
        }
    }
}

fn operation_remaining(request: &native_lifecycle::Request) -> Result<Duration> {
    let deadline = chrono::DateTime::parse_from_rfc3339(&request.expires_at)?;
    let mut remaining = deadline.signed_duration_since(chrono::Utc::now()).to_std()
        .context("Native operation expired")?;
    ensure!(!remaining.is_zero() && remaining <= Duration::from_secs(300), "Native operation expired or unbounded");
    if let native_lifecycle::Action::SwitchVersion { approval_expires_at, .. } = &request.action {
        let approval = chrono::DateTime::parse_from_rfc3339(approval_expires_at)?
            .signed_duration_since(chrono::Utc::now()).to_std().context("Candidate approval expired")?;
        remaining = remaining.min(approval);
    }
    Ok(remaining)
}

/// Fresh post-write producer advancement, not a successful OPC service, confirms
/// the commissioned independent DCS acknowledgement or operator-setting readback.
async fn checked_setting(
    session: &Session, sequence: &opc_interface::NumericReadPlan, ns: u16,
    write_tag: &str, value: f64, feedback_tag: &str, expected: f64,
) -> Result<()> {
    let mut feedback = opc_interface::NumericReadPlan::default();
    feedback.add(&[NodeId::new(ns, feedback_tag)]);
    let mut sequence_values = Vec::with_capacity(1);
    let mut values = Vec::with_capacity(1);
    let before = read_acquisition_snapshot(session, sequence, &feedback, &mut sequence_values, &mut values).await?;
    opc_interface::write_numeric_bulk(session, &[NodeId::new(ns, write_tag)], &[value]).await?;
    loop {
        let after = read_acquisition_snapshot(session, sequence, &feedback, &mut sequence_values, &mut values).await?;
        ensure!(after >= before, "Acquisition producer restarted during acknowledgement");
        if after > before && values[0] == expected { return Ok(()); }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn numeric_operation(
    session: &Session, sequence: &opc_interface::NumericReadPlan, ns: u16,
    profile: &native_lifecycle::Commissioning, action: &native_lifecycle::Action,
) -> Result<()> {
    use native_lifecycle::Action;
    let (id, value) = match action {
        Action::CvTarget { variable_id, value } | Action::OperatingLow { variable_id, value }
        | Action::OperatingHigh { variable_id, value } => (variable_id, *value),
        _ => return Err(anyhow!("Not an operator setting")),
    };
    let variable = profile.variables.iter().find(|v| v.id == *id).context("Unknown commissioned variable")?;
    ensure!(value.is_finite() && value >= variable.hard_low && value <= variable.hard_high, "Operator value outside commissioned hard envelope");
    let low = variable.low_tag.as_deref().context("No commissioned operating-low tag")?;
    let high = variable.high_tag.as_deref().context("No commissioned operating-high tag")?;
    let mut plan = opc_interface::NumericReadPlan::default();
    plan.add(&[NodeId::new(ns, low), NodeId::new(ns, high)]);
    if variable.role == "CV" {
        plan.add(&[NodeId::new(ns, variable.target_tag.as_deref().context("No commissioned CV target")?)]);
    }
    let mut sequence_values = Vec::with_capacity(1); let mut values = Vec::with_capacity(plan.len());
    read_acquisition_snapshot(session, sequence, &plan, &mut sequence_values, &mut values).await?;
    ensure!(values[0] >= variable.hard_low && values[1] <= variable.hard_high && values[0] < values[1], "Invalid current operating bounds");
    let tag = match action {
        Action::CvTarget { .. } => {
            ensure!(variable.role == "CV" && value >= values[0] && value <= values[1], "CV target outside operating bounds");
            variable.target_tag.as_deref().context("No commissioned CV target")?
        }
        Action::OperatingLow { .. } => {
            ensure!(value < values[1] && (variable.role != "CV" || value <= values[2]), "Operating low crosses high or CV target"); low
        }
        Action::OperatingHigh { .. } => {
            ensure!(value > values[0] && (variable.role != "CV" || value >= values[2]), "Operating high crosses low or CV target"); high
        }
        _ => unreachable!(),
    };
    checked_setting(session, sequence, ns, tag, value, tag, value).await
}

/// APC Engine Worker
/// 
/// A dedicated process for running a single Model Predictive Control loop.
/// Controlled via CLI arguments by the 'controller_host'.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the JSON model file (Physics)
    #[arg(short, long)]
    model: String,

    /// Private supervisor JSONL socket; must be paired with an administrator profile.
    #[arg(long)]
    authority_socket: Option<PathBuf>,
    #[arg(long)]
    commissioning_profile: Option<PathBuf>,
    /// Exact immutable approval and manifest for a non-baseline Monitor-only start.
    #[arg(long)]
    startup_approval: Option<PathBuf>,
    /// Explicit baseline Monitor recovery: restore frozen tuning after independent DCS release.
    #[arg(long, requires = "commissioning_profile", conflicts_with_all = ["startup_approval", "live_tuning"])]
    recover_monitor: bool,

    /// Require this lowercase SHA-256 of the exact model bytes parsed, before any OPC connection.
    #[arg(long)]
    expected_model_sha256: Option<String>,

    /// Explicit directory for new manual snapshots, never the immutable model's directory.
    #[arg(long)]
    model_output_dir: Option<PathBuf>,

    /// Explicitly enable manual live weight/alpha tuning even with a byte-verified model.
    /// Do not use for immutable-version launches. Without this flag, a supplied digest
    /// freezes CV weight/alpha and MV weight: any live difference holds the controller.
    #[arg(long)]
    live_tuning: bool,

    /// External acquisition sequence string NodeId (namespace 2). Default absent: control fails
    /// closed. Producer MUST increment to odd before acquiring/publishing the entire
    /// PV/DV/effective-SP process snapshot, then to a newer even integer after completion.
    /// Stable even reads bracket the scan. Observed advancement establishes freshness;
    /// an unchanged counter is usable only within the bounded age of that acquisition.
    /// Operator modes/settings are independently re-read immediately before commands.
    /// This is NOT the engine heartbeat or a last-value-change timestamp. Shared SP
    /// feedback confirms register readback only, not independent physical application.
    #[arg(long)]
    acquisition_sequence_node: Option<String>,

    /// Maximum acquisition evidence age, measured by local monotonic clock since the previous
    /// sequence observation (default: 2 * sample_time). Source/server timestamps are NOT used:
    /// unchanged target/weight timestamps may be arbitrarily old. Missing evidence inhibits Engage.
    #[arg(long)]
    max_acquisition_age_secs: Option<f64>,

    /// Maximum time from starting the control snapshot read until command service completion
    /// (default: sample_time). Timeout means unknown physical outcome and latches Hold.
    /// Independently, observation gaps beyond 1.25 * sample_time reset DMC history and
    /// require a new healthy Monitor solve followed by explicit operator Engage.
    #[arg(long)]
    max_read_to_command_secs: Option<f64>,

    /// Effective-SP confirmation deadline after issuing a vector (default: 2 * sample_time).
    /// No new MV writes while unresolved; partial/rejected writes require Monitor recovery.
    #[arg(long)]
    command_confirmation_secs: Option<f64>,

    /// Absolute engineering-unit effective-SP confirmation tolerance, applied independently
    /// to every MV (not PID OP, percent span, or solver tolerance).
    #[arg(long)]
    command_confirmation_tolerance: Option<f64>,

    /// Contiguous sampled observations per displayed rolling-performance window.
    #[arg(long, default_value_t = 60)]
    monitor_window_samples: usize,

    /// OPC UA Server URL
    #[arg(short, long, default_value = "opc.tcp://127.0.0.1:4855")]
    opc: String,

    /// PKI Root Directory
    #[arg(long, default_value = "pki")]
    pki: String,

    /// OPC UA auth mode: username or x509
    #[arg(long, default_value = "username")]
    auth_mode: String,
    
    /// OPC UA security policy (None or Basic256Sha256)
    #[arg(long, default_value = "Basic256Sha256")]
    security_policy: String,
    
    /// OPC UA message security mode (None, Sign, SignAndEncrypt)
    #[arg(long, default_value = "SignAndEncrypt")]
    message_mode: String,

    /// Username for OPC UA username/password auth
    #[arg(long)]
    username: Option<String>,

    /// Password for OPC UA username/password auth
    #[arg(long)]
    password: Option<String>,
}

fn parse_auth_mode(mode: &str) -> Result<AuthMode> {
    match mode.to_ascii_lowercase().as_str() {
        "username" => Ok(AuthMode::Username),
        "x509" => Ok(AuthMode::X509),
        _ => Err(anyhow!("Invalid --auth-mode '{}'. Use 'username' or 'x509'", mode)),
    }
}

fn parse_security_policy(policy: &str) -> Result<&'static str> {
    match policy.trim().to_ascii_lowercase().as_str() {
        "none" => Ok(SecurityPolicy::None.to_str()),
        "basic256sha256" => Ok(SecurityPolicy::Basic256Sha256.to_str()),
        other => Err(anyhow!(
            "Invalid --security-policy '{}'. Use 'None' or 'Basic256Sha256'",
            other
        )),
    }
}

fn parse_message_mode(mode: &str) -> Result<MessageSecurityMode> {
    match mode.trim().to_ascii_lowercase().as_str() {
        "none" => Ok(MessageSecurityMode::None),
        "sign" => Ok(MessageSecurityMode::Sign),
        "signandencrypt" | "sign_and_encrypt" | "sign-and-encrypt" => {
            Ok(MessageSecurityMode::SignAndEncrypt)
        }
        other => Err(anyhow!(
            "Invalid --message-mode '{}'. Use 'None', 'Sign', or 'SignAndEncrypt'",
            other
        )),
    }
}

/// Save current configuration to new JSON file by reading OPC UA values
async fn save_configuration(
    session: &Session,
    model: &UnifiedModel,
    model_dir: &std::path::Path,
    ns: u16,
) -> Result<String> {
    use chrono::Local;
    
    // Build timestamped filename
    let timestamp = Local::now().format("%Y%m%d_%H%M%S");
    let new_filename = format!("manual_{}_model.json", timestamp);
    let new_path = model_dir.join(&new_filename);
    
    println!("💾 Saving configuration to: {}", new_filename);
    
    // One checked settings read; missing/bad values must not silently retain baseline values.
    let mut nodes = Vec::with_capacity(model.variables.cvs.len() * 7
        + model.variables.mvs.len() * 6 + model.variables.dvs.len() * 2);
    for cv in &model.variables.cvs {
        nodes.push(NodeId::new(ns, format!("{}:Weight", cv.name)));
        nodes.push(NodeId::new(ns, format!("{}:Alpha", cv.name)));
        for id in [&cv.node_ids.target, &cv.node_ids.limits.low, &cv.node_ids.limits.high,
            &cv.node_ids.limits.ll, &cv.node_ids.limits.hh] {
            nodes.push(NodeId::new(ns, id.as_str()));
        }
    }
    for mv in &model.variables.mvs {
        nodes.push(NodeId::new(ns, format!("{}:Weight", mv.name)));
        for id in [&mv.node_ids.limits.low, &mv.node_ids.limits.high,
            &mv.node_ids.limits.ll, &mv.node_ids.limits.hh] {
            nodes.push(NodeId::new(ns, id.as_str()));
        }
        if let Some(id) = &mv.node_ids.target { nodes.push(NodeId::new(ns, id.as_str())); }
    }
    for dv in &model.variables.dvs {
        nodes.push(NodeId::new(ns, dv.node_ids.limits.low.as_str()));
        nodes.push(NodeId::new(ns, dv.node_ids.limits.high.as_str()));
    }
    let mut values = opc_interface::read_bulk(session, &nodes).await?.into_iter();
    let mut next = || values.next().context("Incomplete configuration settings");
    let mut updated_model = model.clone();
    for cv in &mut updated_model.variables.cvs {
        cv.weight = next()?;
        cv.alpha = next()?;
        cv.limits.target = next()?;
        cv.limits.low = next()?;
        cv.limits.high = next()?;
        cv.limits.low_low = next()?;
        cv.limits.high_high = next()?;
        if let config::OptimizationMode::Target { value } = &mut cv.optimization_mode {
            *value = cv.limits.target;
        }
    }
    for mv in &mut updated_model.variables.mvs {
        mv.weight_r = next()?;
        mv.limits.low = next()?;
        mv.limits.high = next()?;
        mv.limits.low_low = next()?;
        mv.limits.high_high = next()?;
        if mv.node_ids.target.is_some() {
            let target = next()?;
            mv.target = Some(target);
            if let config::MvOptimizationMode::Target { value } = &mut mv.optimization_mode {
                *value = target;
            }
        }
    }
    for dv in &mut updated_model.variables.dvs {
        dv.limits.low = next()?;
        dv.limits.high = next()?;
    }
    updated_model.validate_runtime()?;
    
    // Build JSON structure (matching ModelGenerator.tsx format)
    let json_output = json!({
        "control_strategy": updated_model.control_strategy,
        "metadata": {
            "name": updated_model.metadata.name,
            "description": format!("{} (Manual snapshot; not a qualified control version)", updated_model.metadata.description),
            "version": updated_model.metadata.version,
            "model_type": updated_model.metadata.model_type
        },
        "tuning": {
            "prediction_horizon": updated_model.tuning.prediction_horizon,
            "control_horizon": updated_model.tuning.control_horizon,
            "sample_time": updated_model.tuning.sample_time,
            "solver_tolerance": updated_model.tuning.solver_tolerance,
            "max_iterations": updated_model.tuning.max_iterations,
            "terminal_weight_factor": updated_model.tuning.terminal_weight_factor
        },
        "variables": {
            "cvs": updated_model.variables.cvs.iter().map(|cv| json!({
                "name": cv.name,
                "description": cv.description,
                "units": cv.units,
                "weight": cv.weight,
                "alpha": cv.alpha,
                "ece_factor": cv.ece_factor,
                "slack_weight": cv.slack_weight,
                "is_integrating": cv.is_integrating,
                "optimization_mode": match &cv.optimization_mode {
                    config::OptimizationMode::Target { value } => json!({ "type": "Target", "value": value }),
                    config::OptimizationMode::Zone => json!({ "type": "Zone" }),
                    config::OptimizationMode::Maximize => json!({ "type": "Maximize" }),
                    config::OptimizationMode::Minimize => json!({ "type": "Minimize" }),
                },
                "limits": {
                    "low_low": cv.limits.low_low,
                    "low": cv.limits.low,
                    "target": cv.limits.target,
                    "high": cv.limits.high,
                    "high_high": cv.limits.high_high
                },
                "node_ids": {
                    "pv": cv.node_ids.pv,
                    "target": cv.node_ids.target,
                    "prediction": cv.node_ids.prediction,
                    "limits": {
                        "high": cv.node_ids.limits.high,
                        "low": cv.node_ids.limits.low,
                        "hh": cv.node_ids.limits.hh,
                        "ll": cv.node_ids.limits.ll
                    }
                }
            })).collect::<Vec<_>>(),
            "mvs": updated_model.variables.mvs.iter().map(|mv| {
                let mut mv_json = json!({
                    "name": mv.name,
                    "description": mv.description,
                    "units": mv.units,
                    "weight_r": mv.weight_r,
                    "max_move": mv.max_move,
                    "target_weight": mv.target_weight,
                    "optimization_mode": match &mv.optimization_mode {
                        config::MvOptimizationMode::Target { value } => json!({ "type": "Target", "value": value }),
                        config::MvOptimizationMode::Maximize => json!({ "type": "Maximize" }),
                        config::MvOptimizationMode::Minimize => json!({ "type": "Minimize" }),
                    },
                    "limits": {
                        "low_low": mv.limits.low_low,
                        "low": mv.limits.low,
                        "high": mv.limits.high,
                        "high_high": mv.limits.high_high
                    },
                    "node_ids": {
                        "pv": mv.node_ids.pv,
                        "sp": mv.node_ids.sp,
                        "applied_sp": mv.node_ids.applied_sp,
                        "op": mv.node_ids.op,
                        "mode": mv.node_ids.mode,
                        "mode_target": mv.node_ids.mode_target,
                        "future_plan": mv.node_ids.future_plan,
                        "limits": {
                            "high": mv.node_ids.limits.high,
                            "low": mv.node_ids.limits.low,
                            "hh": mv.node_ids.limits.hh,
                            "ll": mv.node_ids.limits.ll
                        }
                    }
                });
                // Add optional economic target
                if let Some(target) = mv.target {
                    mv_json["target"] = json!(target);
                }
                if let Some(target_node) = &mv.node_ids.target {
                    mv_json["node_ids"]["target"] = json!(target_node);
                }
                mv_json
            }).collect::<Vec<_>>(),
            "dvs": updated_model.variables.dvs.iter().map(|dv| json!({
                "name": dv.name,
                "description": dv.description,
                "units": dv.units,
                "limits": {
                    "low": dv.limits.low,
                    "high": dv.limits.high
                },
                "node_ids": {
                    "pv": dv.node_ids.pv,
                    "limits": {
                        "high": dv.node_ids.limits.high,
                        "low": dv.node_ids.limits.low
                    }
                }
            })).collect::<Vec<_>>()
        },
        "physics": {
            "gain": updated_model.physics.gain,
            "tau": updated_model.physics.tau,
            "dead_time": updated_model.physics.dead_time,
            "gain_dv": updated_model.physics.gain_dv,
            "tau_dv": updated_model.physics.tau_dv,
            "dead_time_dv": updated_model.physics.dead_time_dv,
            "step_coefficients": updated_model.physics.step_coefficients,
            "dv_coefficients": updated_model.physics.dv_coefficients
        }
    });
    
    // A manual snapshot never overwrites a selected model or an earlier snapshot.
    use std::io::Write;
    let bytes = serde_json::to_vec_pretty(&json_output)?;
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&new_path)?;
    if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(&new_path);
        return Err(error.into());
    }
    
    println!("✅ Configuration saved to: {}", new_filename);
    Ok(new_filename)
}

/// Load model and initialize controller
fn load_model_and_controller(model_path: &str, expected_sha256: Option<&str>) -> Result<(UnifiedModel, DmcController, String)> {
    let (model, digest) = UnifiedModel::load_verified(model_path, expected_sha256)?;
    println!("✅ Model Loaded: {}", model.metadata.name);
    println!("Control strategy: {:?} v{}", model.control_strategy.method, model.control_strategy.version);

    // Load Step Response Coefficients
    let (step_coefficients, dv_coefficients) = match model.metadata.model_type.as_str() {
        "parametric" => {
            println!("📊 Generating step response from FOPDT parameters...");
            let mv_coeffs = load_parametric_model(&model)?;
            let dv_coeffs = Vec::new();
            (mv_coeffs, dv_coeffs)
        }
        "step_response" => {
            println!("📊 Loading direct step response coefficients...");
            load_stepresponse_model(&model)?
        }
        other => {
            return Err(anyhow!("Unknown model_type: '{}'. Use 'parametric' or 'step_response'", other));
        }
    };

    let controller = DmcController::new_from_coefficients(step_coefficients, dv_coefficients, &model)?;
    Ok((model, controller, digest))
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    
    let args = Args::parse();
    println!("🔐 Starting Industrial DMC (Worker Mode)...");
    let auth_mode = parse_auth_mode(&args.auth_mode)?;
    let security_policy = parse_security_policy(&args.security_policy)?;
    let message_mode = parse_message_mode(&args.message_mode)?;
    
    // Construct AppSettings from Args (Migration from settings.toml)
    let pki_path = PathBuf::from(&args.pki);
    let mut settings = AppSettings {
        opcua: config::OpcuaConfig {
            endpoint_url: args.opc.clone(),
            security_policy: args.security_policy.clone(),
            message_mode: args.message_mode.clone(),
            namespace_index: 2,
        },
        identity: config::IdentityConfig {
            app_name: "AuTHRity APC Engine".to_string(),
            app_uri: "urn:authrity:engine:generic".to_string(), // Will be updated after ID determination
            auto_create_keys: true,
            trust_server_certs: false,
        },
        auth: config::AuthConfig {
            mode: auth_mode,
            username: args.username.clone(),
            password: args.password.clone(),
        },
        runtime: config::RuntimeConfig {
            model_path: args.model.clone(),
            reconnect_delay_sec: 5,
        },
        paths: config::PathConfig {
            pki_dir: args.pki.clone(),
            cert_path: pki_path.join("own/cert.der").to_string_lossy().to_string(),
            key_path: pki_path.join("own/private.pem").to_string_lossy().to_string(),
        }
    };

    // Load initial model
    let (mut model, mut controller, mut controller_sha256) = load_model_and_controller(&settings.runtime.model_path, args.expected_model_sha256.as_deref())?;
    ensure!(args.authority_socket.is_some() == args.commissioning_profile.is_some(),
        "authority-socket and commissioning-profile must be supplied together");
    let commissioning = args.commissioning_profile.as_deref().map(|path|
        native_lifecycle::Commissioning::load(path, &model, &controller_sha256, &args.opc, args.startup_approval.as_deref())).transpose()?;
    ensure!(commissioning.is_none() || !args.live_tuning, "Commissioned immutable authority forbids manual live tuning");
    ensure!(args.startup_approval.is_none() || commissioning.is_some(), "Startup candidate approval requires a commissioned authority profile");
    let writer_locks = match (&commissioning, &args.authority_socket) {
        (Some(profile), Some(socket)) => native_lifecycle::lock_writers(socket, profile)?,
        _ => Vec::new(),
    };
    let acquisition_node = commissioning.as_ref().map(|p| p.acquisition_sequence_node.as_str())
        .or(args.acquisition_sequence_node.as_deref());
    let sample_interval = model.sample_interval()?;
    let max_acquisition_age = config::positive_duration(args.max_acquisition_age_secs.unwrap_or(model.tuning.sample_time * 2.0), "max-acquisition-age-secs")?;
    let max_acquisition_age = commissioning.as_ref().map_or(max_acquisition_age,
        |profile| max_acquisition_age.min(Duration::from_millis(profile.maximum_age_ms)));
    let max_read_to_command = config::positive_duration(args.max_read_to_command_secs.unwrap_or(model.tuning.sample_time), "max-read-to-command-secs")?;
    let max_read_to_command = max_read_to_command.min(max_acquisition_age);
    let acquisition_poll = (max_acquisition_age / 2).min(Duration::from_secs(1)).max(Duration::from_millis(1));
    let confirmation_deadline = config::positive_duration(args.command_confirmation_secs.unwrap_or(model.tuning.sample_time * 2.0), "command-confirmation-secs")?;
    let confirmation_deadline = commissioning.as_ref().map_or(confirmation_deadline,
        |profile| confirmation_deadline.min(Duration::from_millis(profile.handoff.timeout_ms)));
    let confirmation_tolerance = args.command_confirmation_tolerance
        .unwrap_or_else(|| commissioning.as_ref().map_or(0.001, |profile| profile.application_tolerance));
    ensure!(commissioning.as_ref().is_none_or(|profile| confirmation_tolerance <= profile.application_tolerance),
        "Application confirmation tolerance exceeds the independently commissioned engineering-unit bound");
    let confirmation_poll = (sample_interval.min(confirmation_deadline) / 4).max(Duration::from_nanos(1));
    ensure!(confirmation_tolerance.is_finite() && confirmation_tolerance >= 0.0,
        "command-confirmation-tolerance must be finite and nonnegative");
    ensure!(max_read_to_command <= sample_interval, "read-to-command budget must not exceed sample_time");
    // More than 25% scheduling/read jitter invalidates the one-interval FIR observation.
    let discontinuity_limit = sample_interval.checked_add(sample_interval / 4).context("Scan interval overflow")?;

    // Determine Identity: Strictly from Model Name > Filename
    let engine_id = if !model.metadata.name.is_empty() {
        model.metadata.name.clone()
    } else {
        std::path::Path::new(&settings.runtime.model_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("UnknownEngine") 
            .to_string()
    };

    // Update Identity URI based on Engine ID
    settings.identity.app_uri = format!("urn:authrity:engine:{}", engine_id);

    println!("⚙️ Engine ID: {}", engine_id);
    println!("⚙️ Model Path: {}", settings.runtime.model_path);
    println!("⚙️ Target OPC: {}", settings.opcua.endpoint_url);
    println!("⚙️ Security Policy: {}", settings.opcua.security_policy);
    println!("⚙️ Message Mode: {}", settings.opcua.message_mode);


    if settings.auth.mode == AuthMode::Username {
        let user = settings.auth.username.as_deref().map(str::trim);
        let pass = settings.auth.password.as_deref().map(str::trim);
        if user.is_none() || pass.is_none() || user == Some("") || pass == Some("") {
            return Err(anyhow!("Invalid OPC auth config: username mode requires --username and --password"));
        }
    }

    let mut client = ClientBuilder::new()
        .application_name(&settings.identity.app_name)
        .application_uri(&settings.identity.app_uri)
        .pki_dir(pki_path)
        .certificate_path(&settings.paths.cert_path)
        .private_key_path(&settings.paths.key_path)

        .create_sample_keypair(settings.identity.auto_create_keys)
        .trust_server_certs(settings.identity.trust_server_certs)
        // Reconnect only through our outer loop, which resets history and command authority.
        .session_retry_limit(0)
        .client()
        .map_err(|e| anyhow!("Failed to build client: {:?}", e))?;

    let x509_identity = if settings.auth.mode == AuthMode::X509 {
        let cert_bytes = std::fs::read(&settings.paths.cert_path)
            .context(format!("Failed to read cert: {}", settings.paths.cert_path))?;
        let x509 = X509::from_der(&cert_bytes).map_err(|e| anyhow!("Parse Cert Error: {:?}", e))?;

        let key_bytes = std::fs::read(&settings.paths.key_path)
            .context(format!("Failed to read key: {}", settings.paths.key_path))?;
        let private_key = PrivateKey::from_pem(&key_bytes).map_err(|e| anyhow!("Parse Key Error: {:?}", e))?;

        Some((x509, private_key))
    } else {
        None
    };

    let mut initial_connection = true;
    let mut performance = RollingPerformance::new(args.monitor_window_samples,
        model.variables.cvs.len(), model.variables.mvs.len())?;
    let mut observation = RuntimeObservation::new(&engine_id, &model, args.monitor_window_samples);
    let mut observation_json = Vec::with_capacity(512 + model.variables.cvs.len() * 320
        + model.variables.mvs.len() * 128 + model.variables.dvs.len() * 96);
    let instance_id = native_lifecycle::new_instance_id().context("Native instance entropy unavailable")?;
    let mut live_forecast = live_forecast::LiveForecast::new(&model, &engine_id, &instance_id,
        &controller_sha256, commissioning.is_none() && (args.expected_model_sha256.is_none() || args.live_tuning));
    let native = native_lifecycle::Native::new(&instance_id, &controller_sha256, commissioning.as_ref());
    native.own_writers(writer_locks)?;
    native.model_metadata(&model);
    let mut authority_watchdog = 0_u64;
    let mut native_rx = args.authority_socket.as_deref().map(|path| native_lifecycle::listen(path, native.clone())).transpose()?;
    let mut received_operation: Option<native_lifecycle::Envelope> = None;
    let mut active_operation: Option<native_lifecycle::Envelope> = None;
    let mut active_dispatched = false;
    let mut first_move_bound: Option<Vec<f64>> = None;
    if let Some(profile) = &commissioning {
        ensure!(profile.history_capacity_samples >= controller.history_samples().1,
            "Commissioned history capacity must cover the controller's complete MV/DV kernel");
    }
    let mut history = native_handoff::History::new(commissioning.as_ref().map_or(2, |p| p.history_capacity_samples));
    // --- OUTER LOOP: RECONNECTION MANAGER ---
    loop {
        println!("🔄 Connecting to server...");

        let (token_type, identity_token) = match settings.auth.mode {
            AuthMode::Username => (
                UserTokenType::UserName,
                IdentityToken::UserName(
                    settings.auth.username.clone().unwrap_or_default(),
                    settings.auth.password.clone().unwrap_or_default().into(),
                ),
            ),
            AuthMode::X509 => (
                UserTokenType::Certificate,
                {
                    let (x509, private_key) = x509_identity
                        .as_ref()
                        .ok_or_else(|| anyhow!("Missing x509 identity for x509 auth mode"))?;
                    IdentityToken::X509(Box::new(x509.clone()), Box::new(private_key.clone()))
                },
            ),
        };

        let (session, connection_task) = match client.connect_to_matching_endpoint(
            (
                settings.opcua.endpoint_url.as_str(),
                security_policy,
                message_mode,
                UserTokenPolicy {
                    token_type,
                    ..Default::default()
                },
            ),
            identity_token,
        ).await {
            Ok((session, event_loop)) => (session, event_loop.spawn()),
            Err(e) => {
                println!("❌ Connection failed: {}. Retrying in 5s...", e);
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        if !matches!(tokio::time::timeout(Duration::from_secs(10), session.wait_for_connection()).await, Ok(true)) {
            connection_task.abort();
            reset_observations(&mut controller, &mut performance);
            eprintln!("Session did not become connected; no initialization or MV writes were performed");
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        println!("✅ Secure Session Active.");
        reset_observations(&mut controller, &mut performance);
        if initial_connection { native.monitor(); }
        else { native.fault("New OPC session requires explicit confirmed release, Monitor and arm"); }
        history.clear();

        // --- PREPARE NODES (Once per connection) ---
        let ns = settings.opcua.namespace_index;

        // Use Engine ID as absolute system node base (single source of truth)
        let sys_node_base = engine_id.clone();
        println!("🔗 Binding to System Node: {}", sys_node_base);

        // Strict validation: fail fast if model metadata/name and OPC node prefix diverge
        let op_mode_probe = NodeId::new(ns, format!("{}:OperatingMode", sys_node_base));
        if let Err(e) = opc_interface::read_bulk(&session, &[op_mode_probe.clone()]).await {
            return Err(anyhow!(
                "System node prefix mismatch. Expected '{}' from model metadata.name, but '{}' is not readable ({}). Ensure model generator writes identical system node prefix in OPC YAML.",
                sys_node_base,
                format!("{}:OperatingMode", sys_node_base),
                e
            ));
        }

        // ✅ NEW: SYSTEM NODES
        let node_op_mode = NodeId::new(ns, format!("{}:OperatingMode", sys_node_base));
        let node_exec_time = NodeId::new(ns, format!("{}:ExecutionTimeMs", sys_node_base));
        let node_next_run = NodeId::new(ns, format!("{}:NextRun", sys_node_base));
        let node_solver_status = NodeId::new(ns, format!("{}:SolverStatus", sys_node_base));
        let node_heartbeat = NodeId::new(ns, format!("{}:Heartbeat", sys_node_base));
        
        // Configuration Management Nodes
        let node_save_config = NodeId::new(ns, format!("{}:SaveConfiguration", sys_node_base));
        let node_config_status = NodeId::new(ns, format!("{}:ConfigurationStatus", sys_node_base));
        let node_objective_function = NodeId::new(ns, format!("{}:ObjectiveFunction", sys_node_base));
        let node_control_observation = NodeId::new(ns, format!("{}:ControlObservation", sys_node_base));
        let node_forecast_envelope = NodeId::new(ns, format!("{}:ForecastEnvelope", sys_node_base));
        let control_telemetry = ControlTelemetry {
            status: &node_config_status, observation: &node_control_observation, engine_id: &sys_node_base,
            forecast: &node_forecast_envelope,
            native: &native,
        };

        let mut heartbeat_counter: i32 = 0;

        // 1. PVs & Targets
        let pv_nodes: Vec<NodeId> = model.variables.cvs.iter()
            .map(|cv| NodeId::new(ns, cv.node_ids.pv.clone()))
            .collect();
        let target_nodes: Vec<NodeId> = model.variables.cvs.iter()
            .map(|cv| NodeId::new(ns, cv.node_ids.target.clone()))
            .collect();

        // 2. MVs (Read/Write to Setpoint)
        let mv_nodes: Vec<NodeId> = model.variables.mvs.iter()
            .map(|mv| NodeId::new(ns, mv.node_ids.sp.clone())) 
            .collect();
        let mv_feedback_nodes: Vec<NodeId> = model.variables.mvs.iter()
            .map(|mv| NodeId::new(ns, mv.node_ids.applied_sp.as_deref().unwrap_or(mv.node_ids.sp.as_str())))
            .collect();
        // PID OP remains independently observed telemetry; it is never a model input/SP.
        let mv_op_nodes: Vec<NodeId> = model.variables.mvs.iter()
            .map(|mv| NodeId::new(ns, mv.node_ids.op.clone())).collect();
        let mut mv_max_moves: Vec<f64> = model.variables.mvs.iter().map(|mv| mv.max_move).collect();
        let mut command_authority = ControlAuthority::default();
        
        // 2a. MV Modes (Read Only - check if PID accepts RCAS)
        let mv_mode_nodes: Vec<NodeId> = model.variables.mvs.iter()
            .map(|mv| NodeId::new(ns, mv.node_ids.mode.clone()))
            .collect();
        let mv_mode_target_nodes: Vec<NodeId> = model.variables.mvs.iter()
            .map(|mv| NodeId::new(ns, mv.node_ids.mode_target.clone()))
            .collect();
        
        // 2b. MV Economic Targets (Optional - only if target node exists)
        let mv_target_nodes: Vec<Option<NodeId>> = model.variables.mvs.iter()
            .map(|mv| mv.node_ids.target.as_ref().map(|t| NodeId::new(ns, t.clone())))
            .collect();
        
        // 2c. DVs (Read Only)
        let dv_nodes: Vec<NodeId> = model.variables.dvs.iter()
            .map(|dv| NodeId::new(ns, dv.node_ids.pv.clone()))
            .collect();
        
        // 2d. DV Limits (Dynamic Read)
        let dv_high_nodes: Vec<NodeId> = model.variables.dvs.iter()
            .map(|dv| NodeId::new(ns, dv.node_ids.limits.high.clone()))
            .collect();
        let dv_low_nodes: Vec<NodeId> = model.variables.dvs.iter()
            .map(|dv| NodeId::new(ns, dv.node_ids.limits.low.clone()))
            .collect();

        // 3. Visualization Nodes (Future Plan & Prediction)
        let mv_plan_nodes: Vec<NodeId> = model.variables.mvs.iter()
            .map(|mv| NodeId::new(ns, mv.node_ids.future_plan.clone())) 
            .collect();
        let mv_steadystate_nodes: Vec<NodeId> = model.variables.mvs.iter()
            .map(|mv| NodeId::new(ns, format!("{}:SteadyState", mv.name)))
            .collect();
        let mv_lastmove_nodes: Vec<NodeId> = model.variables.mvs.iter()
            .map(|mv| NodeId::new(ns, format!("{}:LastMove", mv.name)))
            .collect();
        let cv_pred_nodes: Vec<NodeId> = model.variables.cvs.iter()
            .map(|cv| NodeId::new(ns, cv.node_ids.prediction.clone())) 
            .collect();
        let cv_steadystate_nodes: Vec<NodeId> = model.variables.cvs.iter()
            .map(|cv| NodeId::new(ns, format!("{}:SteadyState", cv.name)))
            .collect();
        let cv_bias_nodes: Vec<NodeId> = model.variables.cvs.iter()
            .map(|cv| NodeId::new(ns, format!("{}:Bias", cv.name)))
            .collect();

        // 4. Limit Nodes (Dynamic Read)
        let mv_high_nodes: Vec<NodeId> = model.variables.mvs.iter()
            .map(|mv| NodeId::new(ns, mv.node_ids.limits.high.clone())) 
            .collect();
        let mv_low_nodes: Vec<NodeId> = model.variables.mvs.iter()
            .map(|mv| NodeId::new(ns, mv.node_ids.limits.low.clone())) 
            .collect();
        let mv_hh_nodes: Vec<NodeId> = model.variables.mvs.iter()
            .map(|mv| NodeId::new(ns, mv.node_ids.limits.hh.clone())) 
            .collect();
        let mv_ll_nodes: Vec<NodeId> = model.variables.mvs.iter()
            .map(|mv| NodeId::new(ns, mv.node_ids.limits.ll.clone())) 
            .collect();        
        // 5. Operating mode 

        // Runtime values belong to the operator. Never push model-baseline targets, tuning
        // or limits during startup/reconnect; deployment provisions their initial values.
        
        // CV Weight nodes
        let cv_weight_nodes: Vec<NodeId> = model.variables.cvs.iter()
            .map(|cv| NodeId::new(ns, format!("{}:Weight", cv.name)))
            .collect();
        
        // CV Alpha nodes
        let cv_alpha_nodes: Vec<NodeId> = model.variables.cvs.iter()
            .map(|cv| NodeId::new(ns, format!("{}:Alpha", cv.name)))
            .collect();
        
        // CV Limit nodes
        let cv_high_nodes: Vec<NodeId> = model.variables.cvs.iter()
            .map(|cv| NodeId::new(ns, cv.node_ids.limits.high.clone()))
            .collect();
        let cv_low_nodes: Vec<NodeId> = model.variables.cvs.iter()
            .map(|cv| NodeId::new(ns, cv.node_ids.limits.low.clone()))
            .collect();
        let cv_hh_nodes: Vec<NodeId> = model.variables.cvs.iter()
            .map(|cv| NodeId::new(ns, cv.node_ids.limits.hh.clone()))
            .collect();
        let cv_ll_nodes: Vec<NodeId> = model.variables.cvs.iter()
            .map(|cv| NodeId::new(ns, cv.node_ids.limits.ll.clone()))
            .collect();
        
        // MV Weight nodes
        let mv_weight_nodes: Vec<NodeId> = model.variables.mvs.iter()
            .map(|mv| NodeId::new(ns, format!("{}:Weight", mv.name)))
            .collect();
        
        // Normal starts preserve operator tuning; explicit recovery restores it only after DCS release.
        let restore_baseline_tuning = initial_connection && args.recover_monitor;
        if initial_connection {
            tokio::time::timeout(max_read_to_command, opc_interface::write_int32(&session, &node_op_mode, 1))
                .await.context("Startup Monitor reset timed out; outcome unknown")??;
            initial_connection = false;
        }
        let frozen_tuning = commissioning.is_some() || (args.expected_model_sha256.is_some() && !args.live_tuning);
        let policy = if frozen_tuning {
            "Verified model; frozen CV weights/alphas and MV weights (live mismatch inhibits control); targets/limits remain live"
        } else {
            "Explicit manual policy; live CV weights/alphas and MV weights; targets/limits remain live"
        };
        let feedback_policy = if model.variables.mvs.iter().all(|mv| mv.node_ids.applied_sp.is_some()) {
            "separate effective-SP acceptance feedback (not proof of physical actuator motion)"
        } else {
            "includes shared-SP register readback (not independent PID application evidence)"
        };
        let status = format!("{policy}; {feedback_policy}");
        opc_interface::write_string(&session, &node_config_status, &status).await
            .context("Failed to publish effective tuning/feedback policy")?;
        println!("{status}");

        // Request layouts and input buffers are reused; maxAge=0 alone is not freshness.
        let mut reads = opc_interface::NumericReadPlan::default();
        let mode_range = reads.add(std::slice::from_ref(&node_op_mode));
        let pv_range = reads.add(&pv_nodes);
        let target_range = reads.add(&target_nodes);
        let mv_range = reads.add(&mv_feedback_nodes);
        let mv_command_range = commissioning.as_ref().map(|_| reads.add(&mv_nodes));
        let op_range = reads.add(&mv_op_nodes);
        let mv_mode_range = reads.add(&mv_mode_nodes);
        let mv_mode_target_range = reads.add(&mv_mode_target_nodes);
        let dv_range = reads.add(&dv_nodes);
        let dv_high_range = reads.add(&dv_high_nodes);
        let dv_low_range = reads.add(&dv_low_nodes);
        let high_range = reads.add(&mv_high_nodes);
        let low_range = reads.add(&mv_low_nodes);
        let hh_range = reads.add(&mv_hh_nodes);
        let ll_range = reads.add(&mv_ll_nodes);
        let cv_high_range = reads.add(&cv_high_nodes);
        let cv_low_range = reads.add(&cv_low_nodes);
        let cv_hh_range = reads.add(&cv_hh_nodes);
        let cv_ll_range = reads.add(&cv_ll_nodes);
        let cv_weight_range = reads.add(&cv_weight_nodes);
        let cv_alpha_range = reads.add(&cv_alpha_nodes);
        let mv_weight_range = reads.add(&mv_weight_nodes);
        let mut mv_target_ranges = Vec::with_capacity(mv_target_nodes.len());
        for target in &mv_target_nodes {
            mv_target_ranges.push(target.as_ref().map(|node| reads.add(std::slice::from_ref(node))));
        }
        let handoff_range = commissioning.as_ref().map(|profile| reads.add(&[
            NodeId::new(ns, profile.handoff.release_tag.as_str()),
            NodeId::new(ns, profile.handoff.acknowledgement_tag.as_str()),
        ]));
        let baseline_snapshot = |data: &[f64]| -> bool {
            commissioning.as_ref().zip(handoff_range.as_ref()).is_some_and(|(profile, range)|
                data[range.start] == profile.handoff.release_value
                    && data[range.start + 1] == profile.handoff.acknowledgement_value
                    && data[mv_mode_range.clone()].iter().all(|mode| *mode == profile.handoff.local_mode))
        };
        let validate_snapshot = |model: &UnifiedModel, data: &[f64]| -> Result<()> {
            validate_process_group(&data[pv_range.clone()], Some(&data[target_range.clone()]),
                &data[cv_low_range.clone()], &data[cv_high_range.clone()], &data[cv_ll_range.clone()], &data[cv_hh_range.clone()],
                model.variables.cvs.len()).map_err(|e| anyhow!(e))?;
            validate_process_group(&data[mv_range.clone()], None, &data[low_range.clone()], &data[high_range.clone()],
                &data[ll_range.clone()], &data[hh_range.clone()], model.variables.mvs.len()).map_err(|e| anyhow!(e))?;
            validate_process_group(&data[dv_range.clone()], None, &data[dv_low_range.clone()], &data[dv_high_range.clone()],
                &data[dv_low_range.clone()], &data[dv_high_range.clone()], model.variables.dvs.len()).map_err(|e| anyhow!(e))?;
            model.validate_live_tuning(&data[cv_weight_range.clone()], &data[cv_alpha_range.clone()], &data[mv_weight_range.clone()], frozen_tuning)?;
            if let Some(profile) = commissioning.as_ref() {
                for (role, low, high) in [("CV", &cv_ll_range, &cv_hh_range), ("MV", &ll_range, &hh_range), ("DV", &dv_low_range, &dv_high_range)] {
                    for ((v, low), high) in profile.variables.iter().filter(|v| v.role == role).zip(&data[low.clone()]).zip(&data[high.clone()]) {
                        ensure!(*low >= v.hard_low && *high <= v.hard_high, "Live limits expanded beyond commissioned hard envelope");
                    }
                }
            }
            for (i, mv) in model.variables.mvs.iter().enumerate() {
                let target = mv_target_ranges[i].as_ref().map_or(mv.target.unwrap_or(0.0), |range| data[range.start]);
                ensure!(mv.target_weight == 0.0 || (target.is_finite() && target >= data[low_range.start+i] && target <= data[high_range.start+i]),
                    "Invalid economic target for {}", mv.name);
            }
            Ok(())
        };
        let measurement_ranges = [pv_range.clone(), dv_range.clone(), op_range.clone()];
        let mut mv_target_values: Vec<f64> = model.variables.mvs.iter().map(|mv| mv.target.unwrap_or(0.0)).collect();
        let mut values = Vec::with_capacity(reads.len());
        let mut sequence_read = opc_interface::NumericReadPlan::default();
        if let Some(name) = acquisition_node {
            ensure!(!name.trim().is_empty(), "acquisition-sequence-node must not be empty");
            sequence_read.add(&[NodeId::new(ns, name)]);
        }
        if let Some(profile) = commissioning.as_ref() {
            // A restarted process must not renew an old process's remote lease.
            // First establish actual DCS release; this never grants arm/Engage.
            let handoff = &profile.handoff;
            let release = tokio::time::timeout(Duration::from_millis(handoff.timeout_ms),
                checked_setting(&session, &sequence_read, ns, &handoff.release_tag,
                    handoff.release_value, &handoff.acknowledgement_tag, handoff.acknowledgement_value)).await;
            match release {
                Ok(Ok(())) => native.released(false),
                failure => {
                    native.fault("Initial/reconnected independent DCS release is unconfirmed");
                    return Err(anyhow!("Native session cannot become ready without acquired DCS release: {failure:?}"));
                }
            }
        }
        if args.startup_approval.is_some() || restore_baseline_tuning {
            let tuning_nodes: Vec<_> = cv_weight_nodes.iter().chain(&cv_alpha_nodes).chain(&mv_weight_nodes).cloned().collect();
            let tuning_values: Vec<_> = model.variables.cvs.iter().map(|v| v.weight)
                .chain(model.variables.cvs.iter().map(|v| v.alpha))
                .chain(model.variables.mvs.iter().map(|v| v.weight_r)).collect();
            tokio::time::timeout(max_read_to_command, opc_interface::write_numeric_bulk(&session, &tuning_nodes, &tuning_values))
                .await.context("Authorized cold-start tuning mirror unconfirmed")??;
        }
        let mut sequence_values = Vec::with_capacity(1);
        let mut guard_values = Vec::with_capacity(reads.len());
        let mut evidence = AcquisitionEvidence::default();
        let mut pending = PendingCommands::new(mv_nodes.len());
        let mut alignment_nodes = Vec::with_capacity(mv_nodes.len());
        let mut alignment_values = Vec::with_capacity(mv_nodes.len());
        let mut previous_observation: Option<Instant> = None;
        let mut next_tick = tokio::time::Instant::now();
        let mut next_acquisition_poll = tokio::time::Instant::now() + acquisition_poll;
        let mut tick_count = 0_u64;
        let mut command_sequence: Option<f64> = None;
        let mut previous_scan_sequence: Option<f64> = None;
        let mut normalized_actual = vec![0.0; mv_nodes.len()];
        let mut normalized_requested = vec![0.0; mv_nodes.len()];
        match tokio::time::timeout(max_read_to_command,
            observation_unavailable(&session, &control_telemetry, "New engine session; waiting for healthy observations")).await {
            Ok(Ok(())) => {}
            error => eprintln!("Initial observation invalidation unconfirmed: {error:?}"),
        }

        loop {
            if let Some(operation) = active_operation.take() {
                let state = native.status();
                if operation_remaining(&operation.request).is_err() || state.mode == "hold" {
                    native.fault("Lifecycle operation did not reach independently confirmed application before expiry");
                    command_authority.trip(HoldReason::UnconfirmedWrite);
                    native.finish(operation, false, false, "Lifecycle operation failed or expired; authority latched Hold");
                    active_dispatched = false;
                } else if active_dispatched && state.healthy && !state.pending_application && state.mode == "engaged" {
                    native.finish(operation, true, true, "Native authority and acquired independent applied-SP confirmation complete");
                    active_dispatched = false;
                } else {
                    active_operation = Some(operation);
                }
            }
            if let Some(operation) = received_operation.take() {
                use native_lifecycle::Action;
                if native.replayed(&operation.request.operation_id) {
                    let _ = operation.reply.send(native.reply(false, false, "Operation replay rejected; query original operation instead"));
                    continue;
                }
                let validation = native.validate(&operation.request);
                if let Err(error) = validation {
                    native.finish(operation, false, false, error.to_string());
                    continue;
                }
                let interrupt = matches!(operation.request.action, Action::Hold | Action::Exit | Action::Stop);
                if active_operation.is_some() && !interrupt {
                    native.finish(operation, false, false, "Another lifecycle operation is awaiting physical confirmation");
                    continue;
                }
                if interrupt {
                    if let Some(previous) = active_operation.take() {
                        native.finish(previous, false, false, "Superseded by explicit safe release; original physical outcome not inferred");
                    }
                    active_dispatched = false;
                }
                let outcome: Result<bool> = async {
                    let profile = commissioning.as_ref().context("No administrator commissioning profile")?;
                    if !matches!(operation.request.action, Action::Hold | Action::Exit | Action::Stop | Action::Start) {
                        let budget = operation_remaining(&operation.request)?.min(max_read_to_command);
                        let sequence = tokio::time::timeout(budget, read_acquisition_snapshot(&session, &sequence_read,
                            &reads, &mut sequence_values, &mut guard_values)).await.context("Native command-boundary acquisition timed out")??;
                        let acquired_after = evidence.observe(sequence, sequence, Instant::now(), max_acquisition_age).map_err(|e| anyhow!(e))?;
                        validate_snapshot(&model, &guard_values)?;
                        native.acquired(sequence, acquired_after, &guard_values[mv_range.clone()], baseline_snapshot(&guard_values));
                        if matches!(operation.request.action, Action::SwitchVersion { .. }) {
                            normalize_modes(&guard_values[mv_mode_range.clone()], commissioning.as_ref(), &mut normalized_actual);
                            normalize_modes(&guard_values[mv_mode_target_range.clone()], commissioning.as_ref(), &mut normalized_requested);
                            ensure!(normalized_actual.iter().chain(&normalized_requested).all(|v| *v == 3.0),
                                "Candidate handoff lost actual/requested commissioned remote authority");
                        }
                    }
                    match &operation.request.action {
                        Action::Start => {
                            ensure!(!native.status().armed && pending.deadline(confirmation_deadline).is_none(),
                                "Release engaged/pending authority before returning to Monitor");
                            ensure!(native.status().mode != "hold" || native.status().dcs_handoff,
                                "Hold requires independently confirmed DCS release before Monitor recovery");
                            native.monitor();
                            command_authority.trip(HoldReason::AwaitingOperatorReset);
                            Ok(false)
                        }
                        Action::Arm => {
                            ensure!(controller.history_ready(), "Controller history not qualified");
                            ensure!(baseline_snapshot(&guard_values), "Arming requires fresh independent DCS release and actual local modes");
                            native.arm();
                            Ok(false)
                        }
                        Action::Engage => {
                            ensure!(baseline_snapshot(&guard_values), "Engage requires a still-released independent DCS baseline");
                            alignment_values.clear();
                            alignment_values.extend_from_slice(&guard_values[mv_range.clone()]);
                            let alignment_sequence = sequence_values[0];
                            let budget = operation_remaining(&operation.request)?.min(max_read_to_command);
                            tokio::time::timeout(budget, async {
                                // Register alignment happens while the independent
                                // DCS is released, before any remote-mode/acquire request.
                                opc_interface::write_numeric_bulk(&session, &mv_nodes, &alignment_values).await?;
                                loop {
                                    let sequence = read_acquisition_snapshot(&session, &sequence_read, &reads,
                                        &mut sequence_values, &mut guard_values).await?;
                                    let acquired_after = evidence.observe(sequence, sequence, Instant::now(), max_acquisition_age)
                                        .map_err(|error| anyhow!(error))?;
                                    validate_snapshot(&model, &guard_values)?;
                                    ensure!(baseline_snapshot(&guard_values), "DCS baseline changed during pre-acquire alignment");
                                    if sequence > alignment_sequence {
                                        native_handoff::validate_alignment(&alignment_values,
                                            &guard_values[mv_command_range.as_ref().expect("commissioned SP readback").clone()],
                                            &guard_values[mv_range.clone()], confirmation_tolerance)?;
                                        native.acquired(sequence, acquired_after, &guard_values[mv_range.clone()], true);
                                        break Ok::<_, anyhow::Error>(());
                                    }
                                    tokio::time::sleep(Duration::from_millis(20)).await;
                                }
                            }).await.context("Pre-acquire alignment lacks fresh independent released-baseline confirmation")??;
                            let timeout = operation_remaining(&operation.request)?.min(max_read_to_command);
                            tokio::time::timeout(timeout, opc_interface::write_numeric_bulk(&session, &mv_mode_target_nodes,
                                &profile.remote_modes())).await.context("PID mode request timed out; outcome unknown")??;
                            tokio::time::timeout(timeout, opc_interface::write_numeric_bulk(&session,
                                &[NodeId::new(ns, profile.handoff.release_tag.as_str())], &[profile.handoff.acquire_value]))
                                .await.context("DCS acquisition request timed out; outcome unknown")??;
                            command_authority.healthy_monitor();
                            native.engage();
                            first_move_bound = Some(profile.variables.iter().filter(|v| v.role == "MV")
                                .zip(&model.variables.mvs).map(|(v, mv)| v.max_move.expect("validated move bound").min(mv.max_move)).collect());
                            Ok(true)
                        }
                        Action::Hold | Action::Exit | Action::Stop => {
                            command_authority.trip(HoldReason::AwaitingOperatorReset);
                            native.fault("Explicit operator release");
                            native.invalidate_epoch();
                            let h = &profile.handoff;
                            let timeout = operation_remaining(&operation.request)?.min(Duration::from_millis(h.timeout_ms));
                            tokio::time::timeout(timeout, checked_setting(&session, &sequence_read, ns,
                                &h.release_tag, h.release_value, &h.acknowledgement_tag, h.acknowledgement_value))
                                .await.context("Independent DCS release acknowledgement timed out")??;
                            let budget = operation_remaining(&operation.request)?.min(max_read_to_command);
                            tokio::time::timeout(budget, async {
                                loop {
                                    let sequence = read_acquisition_snapshot(&session, &sequence_read, &reads,
                                        &mut sequence_values, &mut guard_values).await?;
                                    validate_snapshot(&model, &guard_values)?;
                                    ensure!(baseline_snapshot(&guard_values), "DCS release ACK does not match current release request and actual local PID modes");
                                    if let Ok(acquired_after) = evidence.observe(sequence, sequence, Instant::now(), max_acquisition_age) {
                                        native.acquired(sequence, acquired_after, &guard_values[mv_range.clone()], true);
                                        break Ok::<_, anyhow::Error>(());
                                    }
                                    tokio::time::sleep(Duration::from_millis(20)).await;
                                }
                            }).await.context("Released baseline lacks fresh independent applied-SP evidence")??;
                            pending = PendingCommands::new(mv_nodes.len());
                            reset_observations(&mut controller, &mut performance);
                            history.clear();
                            native.released(matches!(operation.request.action, Action::Stop));
                            Ok(false)
                        }
                        Action::SwitchVersion { model_file, approved_sha256, approval_id, approval_expires_at, max_first_move } => {
                            ensure!(!approval_id.trim().is_empty() && approved_sha256 != &controller_sha256
                                && std::path::Path::new(model_file).is_absolute(), "Missing separate candidate approval or immutable path");
                            ensure!(chrono::DateTime::parse_from_rfc3339(approval_expires_at)? > chrono::Utc::now(),
                                "Candidate approval expired");
                            let (candidate, mut rebuilt, digest) = load_model_and_controller(model_file, Some(approved_sha256))?;
                            native_handoff::compatible(&model, &candidate, profile)?;
                            ensure!(max_first_move.len() == candidate.variables.mvs.len()
                                && max_first_move.iter().zip(&candidate.variables.mvs)
                                    .all(|(limit, mv)| limit.is_finite() && *limit > 0.0 && *limit <= mv.max_move),
                                "Candidate first-move bound exceeds commissioned envelope");
                            history.reconstruct(&mut rebuilt)?;
                            operation_remaining(&operation.request)?;
                            ensure!(native.status().healthy, "Candidate reconstruction exceeded acquisition freshness");
                            // Candidate weights are immutable model policy, not old
                            // live tuning. Mirror the newly approved policy while
                            // this sole writer is quiescent; partial failure holds.
                            let tuning_nodes: Vec<_> = cv_weight_nodes.iter().chain(&cv_alpha_nodes).chain(&mv_weight_nodes).cloned().collect();
                            let tuning_values: Vec<_> = candidate.variables.cvs.iter().map(|v| v.weight)
                                .chain(candidate.variables.cvs.iter().map(|v| v.alpha))
                                .chain(candidate.variables.mvs.iter().map(|v| v.weight_r)).collect();
                            let budget = operation_remaining(&operation.request)?.min(max_read_to_command);
                            tokio::time::timeout(budget, async {
                                opc_interface::write_numeric_bulk(&session, &tuning_nodes, &tuning_values).await?;
                                let sequence = read_acquisition_snapshot(&session, &sequence_read, &reads, &mut sequence_values, &mut guard_values).await?;
                                let acquired_after = evidence.observe(sequence, sequence, Instant::now(), max_acquisition_age).map_err(|e| anyhow!(e))?;
                                validate_snapshot(&candidate, &guard_values)?;
                                native.acquired(sequence, acquired_after, &guard_values[mv_range.clone()], baseline_snapshot(&guard_values));
                                Ok::<_, anyhow::Error>(())
                            }).await.context("Approved tuning cutover unconfirmed; authority must Hold")??;
                            for (bound, mv) in mv_max_moves.iter_mut().zip(&candidate.variables.mvs) { *bound = mv.max_move; }
                            for (i, mv) in candidate.variables.mvs.iter().enumerate() {
                                if mv_target_ranges[i].is_none() { mv_target_values[i] = mv.target.unwrap_or(0.0); }
                            }
                            model = candidate;
                            controller = rebuilt;
                            controller_sha256 = digest;
                            performance.reset();
                            observation = RuntimeObservation::new(&engine_id, &model, args.monitor_window_samples);
                            live_forecast.rebind(&model, &controller_sha256);
                            native.model_metadata(&model);
                            first_move_bound = Some(max_first_move.clone());
                            native.cutover(&controller_sha256);
                            Ok(true)
                        }
                        Action::CvTarget { .. } | Action::OperatingLow { .. } | Action::OperatingHigh { .. } => {
                            let timeout = operation_remaining(&operation.request)?.min(max_read_to_command);
                            tokio::time::timeout(timeout, async {
                                numeric_operation(&session, &sequence_read, ns, profile, &operation.request.action).await?;
                                let sequence = read_acquisition_snapshot(&session, &sequence_read, &reads, &mut sequence_values, &mut guard_values).await?;
                                let acquired_after = evidence.observe(sequence, sequence, Instant::now(), max_acquisition_age).map_err(|e| anyhow!(e))?;
                                validate_snapshot(&model, &guard_values)?;
                                native.acquired(sequence, acquired_after, &guard_values[mv_range.clone()], baseline_snapshot(&guard_values));
                                Ok::<_, anyhow::Error>(())
                            }).await.context("Operator-setting readback unconfirmed")??;
                            Ok(false)
                        }
                    }
                }.await;
                match outcome {
                    Ok(true) => { active_operation = Some(operation); active_dispatched = false; }
                    Ok(false) => native.finish(operation, true, true, "Native operation independently confirmed"),
                    Err(error) => {
                        command_authority.trip(HoldReason::UnconfirmedWrite);
                        native.fault(error.to_string());
                        native.finish(operation, false, false, error.to_string());
                    }
                }
                continue;
            }
            let wake = pending.deadline(confirmation_deadline).map_or(next_tick.min(next_acquisition_poll), |deadline| {
                next_tick.min(next_acquisition_poll).min(tokio::time::Instant::now() + confirmation_poll)
                    .min(tokio::time::Instant::from_std(deadline))
            });
            tokio::select! {
                _ = tokio::time::sleep_until(wake) => {}
                operation = async {
                    match native_rx.as_mut() {
                        Some(receiver) => receiver.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    received_operation = operation;
                    continue;
                }
            }
            if pending.expire_if_due(Instant::now(), confirmation_deadline) {
                reset_observations(&mut controller, &mut performance);
                hold_control(&session, &control_telemetry, &mut command_authority, HoldReason::UnconfirmedWrite,
                    "Confirmation deadline elapsed without effective-SP evidence").await;
                continue;
            }
            // Confirmation has its own cadence. It must not wait until the model's
            // next scan or feed extra observations into the fixed-period FIR history.
            if tokio::time::Instant::now() < next_tick {
                next_acquisition_poll = tokio::time::Instant::now() + acquisition_poll;
                let budget = pending.io_budget(Instant::now(), max_read_to_command, confirmation_deadline)
                    .min(next_tick.saturating_duration_since(tokio::time::Instant::now()));
                let poll = tokio::time::timeout(budget, async {
                    let sequence = read_acquisition_snapshot(
                        &session, &sequence_read, &reads, &mut sequence_values, &mut guard_values).await?;
                    let acquired_after = evidence.observe(sequence, sequence, Instant::now(), max_acquisition_age)
                        .map_err(|error| anyhow!(error))?;
                    validate_snapshot(&model, &guard_values)?;
                    native.acquired(sequence, acquired_after, &guard_values[mv_range.clone()], baseline_snapshot(&guard_values));
                    if command_sequence.is_some_and(|issued| sequence <= issued) { return Ok(Confirmation::Waiting); }
                    pending.check(&guard_values[mv_range.clone()], Instant::now(),
                        confirmation_tolerance, confirmation_deadline).map_err(|error| anyhow!(error))
                }).await;
                match poll {
                    Ok(Ok(confirmation)) => {
                        normalize_modes(&guard_values[mv_mode_range.clone()], commissioning.as_ref(), &mut normalized_actual);
                        normalize_modes(&guard_values[mv_mode_target_range.clone()], commissioning.as_ref(), &mut normalized_requested);
                        let action = command_authority.evaluate(native.desired_mode(), &normalized_actual, &normalized_requested);
                        if action == ControlAction::Hold {
                            reset_observations(&mut controller, &mut performance);
                            let reason = command_authority.reason().unwrap_or(HoldReason::InvalidMode);
                            hold_control(&session, &control_telemetry, &mut command_authority, reason,
                                "Operator/PID authority is not valid during confirmation").await;
                        } else if confirmation == Confirmation::Applied {
                            native.applied(action == ControlAction::Control);
                            println!("Effective-SP feedback confirmed; no new command issued between model scans");
                        }
                    }
                    // A read-only poll that exhausts its inter-scan slot yields to
                    // the normal scan. No confirmation is invented; the pending
                    // command and its original deadline remain in force.
                    Err(_) => {}
                    failure => {
                        reset_observations(&mut controller, &mut performance);
                        hold_control(&session, &control_telemetry, &mut command_authority, HoldReason::UnconfirmedWrite,
                            format!("Effective-SP confirmation failed: {failure:?}")).await;
                    }
                }
                continue;
            }
            let tick_start = Instant::now();
            next_tick = tokio::time::Instant::now() + sample_interval;
            next_acquisition_poll = tokio::time::Instant::now() + acquisition_poll;
            // Configuration snapshots must never delay an unresolved command's deadline.
            let should_save = pending.deadline(confirmation_deadline).is_none()
                && matches!(opc_interface::read_bool(&session, &node_save_config).await, Ok(true));
            if should_save {
                if let Err(error) = opc_interface::write_bool(&session, &node_save_config, false).await {
                    eprintln!("Cannot acknowledge configuration save request: {error}");
                } else {
                    let saved = match args.model_output_dir.as_deref() {
                        Some(directory) => save_configuration(&session, &model, directory, ns).await,
                        None => Err(anyhow::anyhow!("Set --model-output-dir to a manual-model directory; immutable artifact directories are not export destinations")),
                    };
                    let message = match saved {
                        Ok(file) => format!("Saved manual model {file}; {status}"),
                        Err(error) => format!("Save failed: {error}; {status}"),
                    };
                    if let Err(error) = opc_interface::write_string(&session, &node_config_status, &message).await {
                        eprintln!("Cannot publish save result: {error}");
                    }
                }
            }
            if sequence_read.len() == 0 {
                reset_observations(&mut controller, &mut performance);
                hold_control(&session, &control_telemetry, &mut command_authority,
                    HoldReason::StaleAcquisition, "No external acquisition sequence configured; commands inhibited").await;
                continue;
            }
            let read_start = Instant::now();
            let snapshot_budget = pending.io_budget(Instant::now(), max_read_to_command, confirmation_deadline);
            let snapshot = tokio::time::timeout(snapshot_budget, async {
                loop {
                    let sequence = read_acquisition_snapshot(&session, &sequence_read, &reads, &mut sequence_values, &mut values).await?;
                    if previous_scan_sequence.is_none_or(|previous| sequence > previous) { break Ok::<_, anyhow::Error>(sequence); }
                    ensure!(previous_scan_sequence.is_none_or(|previous| sequence >= previous), "Acquisition producer restarted");
                    tokio::task::yield_now().await;
                }
            }).await;
            let sequence_after = match snapshot {
                Ok(Ok(sequence)) => sequence,
                failure => {
                    reset_observations(&mut controller, &mut performance);
                    hold_control(&session, &control_telemetry, &mut command_authority,
                        HoldReason::InvalidInput, format!("Incomplete/uncertain control snapshot: {failure:?}")).await;
                    break;
                }
            };
            let acquired_after = match evidence.observe(sequence_after, sequence_after, Instant::now(), max_acquisition_age) {
                Ok(observed) => observed,
                Err(error) => {
                    reset_observations(&mut controller, &mut performance);
                    if previous_observation.is_none() && native.status().mode == "monitor" {
                        // First observation cannot prove producer advancement. It is
                        // warm-up, never armable, rather than an automatic fault reset.
                        continue;
                    }
                    hold_control(&session, &control_telemetry, &mut command_authority, HoldReason::StaleAcquisition, error).await;
                    continue;
                }
            };
            previous_scan_sequence = Some(sequence_after);
            let requested_mode = native.desired_mode();
            let pvs = &values[pv_range.clone()];
            let targets = &values[target_range.clone()];
            let current_mvs = &values[mv_range.clone()];
            let pid_ops = &values[op_range.clone()];
            let mv_modes = &values[mv_mode_range.clone()];
            let mv_mode_targets = &values[mv_mode_target_range.clone()];
            let current_dvs = &values[dv_range.clone()];
            let dv_high = &values[dv_high_range.clone()];
            let dv_low = &values[dv_low_range.clone()];
            let lim_h = &values[high_range.clone()];
            let lim_l = &values[low_range.clone()];
            let lim_hh = &values[hh_range.clone()];
            let lim_ll = &values[ll_range.clone()];
            let cv_lim_h = &values[cv_high_range.clone()];
            let cv_lim_l = &values[cv_low_range.clone()];
            let cv_lim_hh = &values[cv_hh_range.clone()];
            let cv_lim_ll = &values[cv_ll_range.clone()];
            let cv_weights = &values[cv_weight_range.clone()];
            let cv_alphas = &values[cv_alpha_range.clone()];
            let mv_weights = &values[mv_weight_range.clone()];
            for (i, range) in mv_target_ranges.iter().enumerate() {
                if let Some(range) = range { mv_target_values[i] = values[range.start]; }
            }
            let valid = validate_snapshot(&model, &values);
            if let Err(error) = valid {
                reset_observations(&mut controller, &mut performance);
                hold_control(&session, &control_telemetry, &mut command_authority, HoldReason::InvalidInput, error).await;
                continue;
            }
            let observed_now = Instant::now();
            let observation_timestamp = chrono::Utc::now();
            if previous_observation.is_some_and(|last| observed_now.duration_since(last) > discontinuity_limit) {
                reset_observations(&mut controller, &mut performance);
                hold_control(&session, &control_telemetry, &mut command_authority, HoldReason::DiscontinuousScan,
                    "Observation gap exceeded 1.25 sample intervals; monitor again before Engage").await;
            }
            previous_observation = Some(observed_now);
            normalize_modes(mv_modes, commissioning.as_ref(), &mut normalized_actual);
            normalize_modes(mv_mode_targets, commissioning.as_ref(), &mut normalized_requested);
            let action = command_authority.evaluate(requested_mode, &normalized_actual, &normalized_requested);
            if action == ControlAction::Hold {
                reset_observations(&mut controller, &mut performance);
                let reason = command_authority.reason().unwrap_or(HoldReason::InvalidMode);
                hold_control(&session, &control_telemetry, &mut command_authority, reason,
                    "Healthy Monitor solve followed by explicit Engage is required").await;
                continue;
            }
            let checked_confirmation = if pending.deadline(confirmation_deadline).is_some()
                && command_sequence.is_some_and(|issued| sequence_after <= issued) { Ok(Confirmation::Waiting) }
                else { pending.check(current_mvs, Instant::now(), confirmation_tolerance, confirmation_deadline) };
            let confirmation = match checked_confirmation {
                Ok(result) => result,
                Err(error) => {
                    reset_observations(&mut controller, &mut performance);
                    hold_control(&session, &control_telemetry, &mut command_authority, HoldReason::UnconfirmedWrite, error).await;
                    continue;
                }
            };
            if confirmation == Confirmation::Applied {
                native.applied(action == ControlAction::Control);
                println!("Issued vector confirmed against {feedback_policy}");
            }
            if action == ControlAction::Idle {
                reset_observations(&mut controller, &mut performance);
                let _ = tokio::time::timeout(max_read_to_command,
                    observation_unavailable(&session, &control_telemetry, "Controller is off")).await;
                continue;
            }
            let solve_start = Instant::now();
            let result = controller.next_move(pvs, targets, current_mvs, current_dvs, &mv_target_values,
                lim_l, lim_h, cv_lim_l, cv_lim_h, cv_weights, cv_alphas, mv_weights);
            let solved_at = Instant::now();
            let forecast_issued_at = chrono::Utc::now();
            let exec_ms = solve_start.elapsed().as_secs_f64() * 1000.0;
            let solved = matches!(result.status, clarabel::solver::SolverStatus::Solved | clarabel::solver::SolverStatus::AlmostSolved);
            if !solved {
                reset_observations(&mut controller, &mut performance);
                hold_control(&session, &control_telemetry, &mut command_authority, HoldReason::SolverFailure,
                    format!("Solver returned {:?}", result.status)).await;
                continue;
            }
            if pending.expire_if_due(Instant::now(), confirmation_deadline) {
                reset_observations(&mut controller, &mut performance);
                hold_control(&session, &control_telemetry, &mut command_authority, HoldReason::UnconfirmedWrite,
                    "Confirmation deadline elapsed during observation solve").await;
                continue;
            }
            if read_start.elapsed() >= max_read_to_command || acquired_after.elapsed() > max_acquisition_age {
                reset_observations(&mut controller, &mut performance);
                hold_control(&session, &control_telemetry, &mut command_authority, HoldReason::StaleAcquisition,
                    "Solve exceeded acquisition/read budget; not a healthy Monitor scan").await;
                continue;
            }
            // A completed Monitor solve establishes numerical readiness only;
            // private native IPC still requires explicit arm followed by Engage.
            // The same whole-input re-read protects command dispatch from changed limits,
            // settings and operator modes, which are not owned by the acquisition publisher.
            let remaining = pending.io_budget(Instant::now(),
                max_read_to_command.saturating_sub(read_start.elapsed()), confirmation_deadline);
            let guard = tokio::time::timeout(remaining, async {
                let latest_sequence = read_acquisition_snapshot(
                    &session, &sequence_read, &reads, &mut sequence_values, &mut guard_values).await?;
                ensure!(latest_sequence >= sequence_after, "Acquisition producer restarted during solve");
                evidence.observe(latest_sequence, latest_sequence, Instant::now(), max_acquisition_age)
                    .map_err(|error| anyhow!(error))?;
                ensure!(dispatch_contract_matches(&values, &guard_values, &measurement_ranges),
                    "Command coordinates, limits, tuning, targets or operator authority changed during solve");
                validate_process_group(&guard_values[pv_range.clone()], Some(targets),
                    cv_lim_l, cv_lim_h, cv_lim_ll, cv_lim_hh, pv_nodes.len()).map_err(|error| anyhow!(error))?;
                validate_process_group(&guard_values[dv_range.clone()], None,
                    dv_low, dv_high, dv_low, dv_high, dv_nodes.len()).map_err(|error| anyhow!(error))?;
                ensure!(read_start.elapsed() < max_read_to_command && acquired_after.elapsed() < max_acquisition_age,
                    "Acquisition/read-to-command age exceeded");
                Ok::<_, anyhow::Error>(())
            }).await;
            if !matches!(guard, Ok(Ok(()))) {
                reset_observations(&mut controller, &mut performance);
                hold_control(&session, &control_telemetry, &mut command_authority, HoldReason::StaleAcquisition,
                    format!("Post-solve guard failed: {guard:?}")).await;
                continue;
            }
            if action == ControlAction::Observe && confirmation != Confirmation::Waiting && controller.history_ready() {
                command_authority.healthy_monitor();
            }
            history.observe(pvs, current_mvs, current_dvs, controller.history_samples().0);
            native.acquired(sequence_after, acquired_after, current_mvs, baseline_snapshot(&values));
            native.observe(controller.history_ready(), controller.history_samples(), confirmation == Confirmation::Waiting,
                solved_at, forecast_issued_at, sequence_after);
            if let Some(profile) = commissioning.as_ref().filter(|_| native.renews_watchdog()) {
                authority_watchdog = authority_watchdog.checked_add(1).context("Authority watchdog counter exhausted")?;
                ensure!(authority_watchdog < (1_u64 << 53), "Authority watchdog counter exhausted");
                let budget = max_read_to_command.saturating_sub(read_start.elapsed());
                if !matches!(tokio::time::timeout(budget, opc_interface::write_single(&session,
                    &NodeId::new(ns, profile.handoff.watchdog_tag.as_str()), authority_watchdog as f64)).await, Ok(Ok(()))) {
                    hold_control(&session, &control_telemetry, &mut command_authority, HoldReason::UnconfirmedWrite,
                        "Native watchdog publication unconfirmed").await;
                    continue;
                }
            }
            let limits = CommandLimits { low: lim_l, high: lim_h, low_low: lim_ll, high_high: lim_hh,
                max_move: &mv_max_moves, tolerance: model.tuning.solver_tolerance };
            let mut commands = result.next_move;
            let wants_write = matches!(action, ControlAction::Control | ControlAction::Align)
                && confirmation != Confirmation::Waiting && controller.history_ready();
            if wants_write {
                if let Some(bounds) = first_move_bound.as_ref() {
                    if let Err(error) = native_handoff::validate_first_action(&model, &commands, &result.predicted_pvs, bounds) {
                        hold_control(&session, &control_telemetry, &mut command_authority, HoldReason::InvalidCommand, error).await;
                        reset_observations(&mut controller, &mut performance);
                        continue;
                    }
                }
                if action == ControlAction::Align {
                    ensure!(commands.len() == mv_nodes.len(), "Solver command dimensions changed");
                    commands.fill(0.0);
                }
                if let Err(error) = limits.absolute_commands(current_mvs, &mut commands) {
                    reset_observations(&mut controller, &mut performance);
                    hold_control(&session, &control_telemetry, &mut command_authority, HoldReason::InvalidCommand, error).await;
                    continue;
                }
                if action == ControlAction::Align && model.variables.mvs.iter().enumerate()
                    .any(|(i,mv)| normalized_actual[i] != 3.0 && mv.node_ids.applied_sp.is_none()) {
                    hold_control(&session, &control_telemetry, &mut command_authority, HoldReason::InvalidInput,
                        "Align requires separate effective-SP feedback for every transitioning PID").await;
                    reset_observations(&mut controller, &mut performance);
                    continue;
                }
                alignment_nodes.clear();
                alignment_values.clear();
                if action == ControlAction::Align {
                    for i in 0..mv_nodes.len() {
                        if normalized_actual[i] != 3.0 {
                            alignment_nodes.push(mv_nodes[i].clone());
                            alignment_values.push(commands[i]);
                        }
                    }
                }
                let remaining = max_read_to_command.saturating_sub(read_start.elapsed())
                    .min(max_acquisition_age.saturating_sub(acquired_after.elapsed()))
                    .min(confirmation_deadline);
                let remaining = if let Some(operation) = active_operation.as_ref() {
                    match operation_remaining(&operation.request) {
                        Ok(operation_budget) => remaining.min(operation_budget),
                        Err(error) => {
                            hold_control(&session, &control_telemetry, &mut command_authority, HoldReason::InvalidCommand, error).await;
                            continue;
                        }
                    }
                } else { remaining };
                if remaining.is_zero() {
                    reset_observations(&mut controller, &mut performance);
                    hold_control(&session, &control_telemetry, &mut command_authority, HoldReason::StaleAcquisition,
                        "No command age budget remains").await;
                    continue;
                }
                pending.begin(&commands, current_mvs, Instant::now()).map_err(|e| anyhow!(e))?;
                command_sequence = sequence_values.first().copied();
                native.dispatched();
                active_dispatched = active_operation.is_some();
                first_move_bound = None;
                let write = if action == ControlAction::Align {
                    tokio::time::timeout(remaining, opc_interface::write_bulk(&session, &alignment_nodes, &alignment_values)).await
                } else {
                    tokio::time::timeout(remaining, opc_interface::write_bulk(&session, &mv_nodes, &commands)).await
                };
                if !matches!(write, Ok(Ok(()))) || read_start.elapsed() > max_read_to_command || acquired_after.elapsed() > max_acquisition_age {
                    reset_observations(&mut controller, &mut performance);
                    hold_control(&session, &control_telemetry, &mut command_authority, HoldReason::UnconfirmedWrite,
                        format!("Rejected/partial/timed-out write; application unknown: {write:?}")).await;
                    continue;
                }
                println!("Server accepted vector; awaiting {feedback_policy}");
            }
            let forecast_sequence = live_forecast.sequence().checked_add(1);
            let forecast_payload = if controller.history_ready() {
                live_forecast.encode(forecast_issued_at, result.status, &result.predicted_pvs, current_mvs, &result.future_plan)
            } else { Err(anyhow!("Native MV/DV history is not yet qualified; no steady-state shortcut forecast")) };
            if forecast_payload.is_ok() { native.forecast_sequence(forecast_sequence.expect("encoder checked sequence overflow"), forecast_issued_at); }
            if let Err(error) = &forecast_payload {
                eprintln!("Native forecast unavailable: {error}");
            }
            let observation_result = (|| -> Result<()> {
                performance.observe(requested_mode, pvs, targets, cv_lim_l, cv_lim_h, current_mvs)?;
                observation.update(observation_timestamp, requested_mode, &performance,
                    pvs, targets, cv_lim_l, cv_lim_h, current_mvs, current_dvs)?;
                observation_json.clear();
                serde_json::to_writer(&mut observation_json, &observation)?;
                Ok(())
            })();
            if let Err(error) = &observation_result {
                performance.reset();
                eprintln!("Measured-performance observation unavailable: {error}");
            }
            // Advisory telemetry is never used as command acknowledgement.
            let telemetry_budget = pending.io_budget(Instant::now(), max_read_to_command, confirmation_deadline)
                .min(if pending.deadline(confirmation_deadline).is_some() { confirmation_poll } else { max_read_to_command })
                .min(next_tick.saturating_duration_since(tokio::time::Instant::now()));
            let telemetry = tokio::time::timeout(telemetry_budget, async {
                // A single checked String write owns all horizon metadata and arrays.
                // These are advisory plans, not proof that a PID accepted any move.
                let payload = forecast_payload.as_ref().map_or("null", |payload| *payload);
                if let Err(error) = opc_interface::write_string(&session, &node_forecast_envelope, payload).await {
                    eprintln!("Native forecast publication unconfirmed: {error}");
                }
                let state = if wants_write {
                    "Command accepted by server; awaiting effective-SP confirmation"
                } else if confirmation == Confirmation::Waiting {
                    "Awaiting effective-SP confirmation; replacement commands inhibited"
                } else if action == ControlAction::Observe && controller.history_ready() {
                    "Monitor history qualified; explicit arm and Engage required"
                } else if action == ControlAction::Observe {
                    "Monitor rebuilding new-model MV/DV history; command authority inhibited"
                } else {
                    "No command issued"
                };
                let _ = opc_interface::write_string(&session, &node_config_status, state).await;
                if observation_result.is_ok() {
                    if let Ok(payload) = std::str::from_utf8(&observation_json) {
                        if let Err(error) = opc_interface::write_string(&session, &node_control_observation, payload).await {
                            eprintln!("Measured-performance telemetry failed: {error}");
                        }
                    }
                } else {
                    let _ = observation_unavailable(&session, &control_telemetry, "Observation arithmetic or dimensions invalid").await;
                }
                if wants_write {
                    for i in 0..commands.len() {
                        let _ = opc_interface::write_single(&session, &mv_lastmove_nodes[i], commands[i] - current_mvs[i]).await;
                    }
                }
                let _ = opc_interface::write_single(&session, &node_exec_time, exec_ms).await;
                let solver_code = if result.status == clarabel::solver::SolverStatus::Solved { 0 } else { 1 };
                let _ = opc_interface::write_int32(&session, &node_solver_status, solver_code).await;
                if forecast_payload.is_ok() {
                    for i in 0..mv_nodes.len() {
                        let mut running = current_mvs[i];
                        let plan: Vec<f64> = (0..model.tuning.control_horizon).map(|step| {
                            running += result.future_plan[step * mv_nodes.len() + i];
                            running
                        }).collect();
                        let last = plan.last().copied();
                        let _ = opc_interface::write_array(&session, &mv_plan_nodes[i], plan).await;
                        if let Some(value) = last { let _ = opc_interface::write_single(&session, &mv_steadystate_nodes[i], value).await; }
                    }
                    for i in 0..pv_nodes.len() {
                        let trajectory: Vec<f64> = (0..model.tuning.prediction_horizon)
                            .map(|step| result.predicted_pvs[step * pv_nodes.len() + i]).collect();
                        let last = trajectory.last().copied();
                        let _ = opc_interface::write_array(&session, &cv_pred_nodes[i], trajectory).await;
                        if let Some(value) = last { let _ = opc_interface::write_single(&session, &cv_steadystate_nodes[i], value).await; }
                        let _ = opc_interface::write_single(&session, &cv_bias_nodes[i], result.cv_bias[i]).await;
                    }
                }
                let _ = opc_interface::write_single(&session, &node_objective_function, result.objective_value).await;
                heartbeat_counter = heartbeat_counter.wrapping_add(1);
                let _ = opc_interface::write_int32(&session, &node_heartbeat, heartbeat_counter).await;
                let remaining_seconds = sample_interval.saturating_sub(tick_start.elapsed()).as_secs_f64().ceil().min(i32::MAX as f64) as i32;
                let _ = opc_interface::write_int32(&session, &node_next_run, remaining_seconds).await;
            }).await;
            if telemetry.is_err() { eprintln!("Advisory telemetry timed out; command confirmation retains priority"); }
            tick_count += 1;
            if tick_count % 60 == 1 {
                println!("Action={action:?}; effective SP={current_mvs:?}; PID OP telemetry={pid_ops:?}; solve={exec_ms:.1}ms");
            }
        }

        connection_task.abort();
        println!("⏳ Waiting 5s before reconnecting...");
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

#[cfg(test)]
mod startup_tests {
    use super::Args;
    use clap::{error::ErrorKind, Parser};

    #[test]
    fn baseline_recovery_rejects_uncommissioned_manual_and_candidate_starts() {
        let base = ["engine", "--model", "model.json", "--recover-monitor"];
        assert_eq!(Args::try_parse_from(base).unwrap_err().kind(), ErrorKind::MissingRequiredArgument);
        for forbidden in [&["--startup-approval", "approval.json"][..], &["--live-tuning"][..]] {
            let mut args = base.to_vec();
            args.extend(["--commissioning-profile", "profile.json", "--authority-socket", "authority.sock"]);
            args.extend_from_slice(forbidden);
            assert_eq!(Args::try_parse_from(args).unwrap_err().kind(), ErrorKind::ArgumentConflict);
        }
    }
}