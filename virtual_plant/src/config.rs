// src/config.rs
use serde::Deserialize;
use anyhow::{Context, Result};

// --- RUNTIME SETTINGS (FROM settings.toml) ---
#[derive(Debug, Deserialize, Clone)]
pub struct AppSettings {
    pub opcua: OpcuaConfig,
    pub identity: IdentityConfig,
    pub auth: AuthConfig,
    pub runtime: RuntimeConfig,
    pub debutanizer: DebutanizerConfig,
    pub cstr: CstrConfig,
    pub paths: PathConfig,
    pub acquisition: AcquisitionConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct OpcuaConfig {
    pub endpoint_url: String,
    pub security_policy: String,
    pub message_mode: String,
    pub namespace_index: u16,
}

/// Required producer bindings: namespace-2 String IDs, never inferred from heartbeat.
#[derive(Debug, Deserialize, Clone)]
pub struct AcquisitionConfig {
    pub debutanizer: String,
    pub cstr: String,
    pub pass_balancing: String,
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
    pub speed_multiplier: usize,
    pub reconnect_delay_sec: u64,
    pub cycle_time_ms: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DebutanizerConfig {
    pub num_stages: usize,
    pub feed_stage: usize,
    pub relative_volatility: f64,
    pub hold_up_molar: f64,
    pub dt_seconds: f64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CstrConfig {
    // Empty for now - CSTR uses defaults
}

#[derive(Debug, Deserialize, Clone)]
pub struct PathConfig {
    pub pki_dir: String,
    pub cert_path: String,
    pub key_path: String,
}

impl AppSettings {
    pub fn load(path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .context("Failed to read settings.toml")?;
        let settings: Self = toml::from_str(&content)
            .context("Failed to parse TOML (all three acquisition node IDs are required)")?;
        anyhow::ensure!(settings.opcua.namespace_index == 2,
            "Simulator acquisition requires OPC UA namespace 2");
        let nodes = [
            settings.acquisition.debutanizer.as_str(),
            settings.acquisition.cstr.as_str(),
            settings.acquisition.pass_balancing.as_str(),
        ];
        for (i, node) in nodes.iter().enumerate() {
            anyhow::ensure!(
                node.trim() == *node
                    && node.strip_suffix(":AcquisitionSequence").is_some_and(|id| !id.is_empty()),
                "acquisition node must be a nonempty controller String ID ending in :AcquisitionSequence"
            );
            anyhow::ensure!(!nodes[..i].contains(node), "acquisition node IDs must be distinct");
        }
        anyhow::ensure!(settings.runtime.speed_multiplier > 0, "speed_multiplier must be positive");
        Ok(settings)
    }
}