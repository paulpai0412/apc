// src/main.rs

mod config;
mod opc_interface;
mod plant; 

use anyhow::{Result, anyhow, Context};
use std::path::PathBuf;
use std::time::Duration;

use opcua::{
    client::{ClientBuilder, IdentityToken},
    crypto::{SecurityPolicy, X509, PrivateKey},
    types::{MessageSecurityMode, NodeId, UserTokenPolicy, UserTokenType, Variant},
};

use plant::debutanizer::{DebutanizerModel, ColumnConfig, ModelInputs, MW, DENSITY};
use plant::cstr::{CSTRModel, CSTRInputs};
use plant::pass_balancing::{PassBalancingConfig, PassBalancingInputs, PassBalancingModel};
use config::{AppSettings, AuthMode};

// --- NODE NAME CONSTANTS ---

// VP_Debutanizer Nodes
const NODE_REFLUX_SP: &str = "Reflux:SP"; 
const NODE_REFLUX_PV: &str = "Reflux:PV";
const NODE_REFLUX_OP: &str = "Reflux:OP";
const NODE_STEAM_SP: &str  = "Steam:SP";   
const NODE_STEAM_PV: &str  = "Steam:PV";
const NODE_STEAM_OP: &str  = "Steam:OP";
const NODE_FEED_DV: &str   = "Feed_Flow:PV"; 
const NODE_TOP_C4_CV: &str = "Top_C4:PV";      
const NODE_BTM_TEMP_CV: &str = "Btm_Temp:PV";

// VP_CSTR Nodes
const NODE_CSTR_COOLING_SP: &str = "CSTR_Cooling:SP"; 
const NODE_CSTR_COOLING_PV: &str = "CSTR_Cooling:PV";
const NODE_CSTR_COOLING_OP: &str = "CSTR_Cooling:OP"; 
const NODE_CSTR_FEED_DV: &str    = "CSTR_Feed:PV";    
const NODE_CSTR_TEMP_CV: &str    = "CSTR_Temp:PV";    
const NODE_CSTR_CONC_CV: &str    = "CSTR_Conc:PV"; 

// VP_PassBalancing Nodes
const NODE_FLOW_MASTER_OP: &str = "FlowMaster:OP";
const NODE_FLOW_MASTER_PV: &str = "FlowMaster:PV";
const NODE_COKE_ENABLE: &str = "PassBalancing:CokeEnable";
const NODE_DISTURBANCE_ENABLE: &str = "PassBalancing:DisturbanceEnable";
const NODE_DISTURBANCE_CV_PV: &str = "DisturbanceEnable:PV";
const NODE_DISTURBANCE_CV_TARGET: &str = "DisturbanceEnable:Target";
const NODE_SUM_BIAS: &str = "PassBalancing:Sum_Bias";
const NODE_SUM_BIAS_CV_PV: &str = "Sum_Bias:PV";


#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    println!("Starting AuTHRity: Combined Plant Simulator...");

    // 1. Load Configuration
    let settings_path = "config/settings.toml";
    let settings = AppSettings::load(settings_path)
        .with_context(|| format!("Failed to load config from {}", settings_path))?;
    
    println!("Configuration loaded:");
    println!("   - OPC UA: {}", settings.opcua.endpoint_url);
    println!("   - App: {} ({})", settings.identity.app_name, settings.identity.app_uri);
    println!("   - Speed: {}x real-time", settings.runtime.speed_multiplier);
    println!("   - CSTR Config: {:?}", settings.cstr);

    // 2. Setup Paths & Certificates
    let project_root = PathBuf::from(".");
    let pki_dir = project_root.join(&settings.paths.pki_dir);
    let cert_path = project_root.join(&settings.paths.cert_path);
    let key_path = project_root.join(&settings.paths.key_path);

    // 3. Configure Client
    let mut client_builder = ClientBuilder::new()
        .application_name(&settings.identity.app_name)
        .application_uri(&settings.identity.app_uri)
        .pki_dir(pki_dir)
        .certificate_path(&cert_path)
        .private_key_path(&key_path)
        .create_sample_keypair(settings.identity.auto_create_keys)
        .trust_server_certs(settings.identity.trust_server_certs)
        .client()
        .map_err(|e| anyhow!("Config error: {:?}", e))?;

    let cert_bytes = std::fs::read(&cert_path).context("Read cert.der")?;
    let x509 = X509::from_der(&cert_bytes).map_err(|e| anyhow!("Parse Cert: {:?}", e))?;
    let key_bytes = std::fs::read(&key_path).context("Read private.pem")?;
    let private_key = PrivateKey::from_pem(&key_bytes).map_err(|e| anyhow!("Parse Key: {:?}", e))?;

    if settings.auth.mode == AuthMode::Username {
        let user = settings.auth.username.as_deref().map(str::trim);
        let pass = settings.auth.password.as_deref().map(str::trim);
        if user.is_none() || pass.is_none() || user == Some("") || pass == Some("") {
            return Err(anyhow!("Invalid OPC auth config: username mode requires non-empty auth.username and auth.password"));
        }
    }

    if settings.auth.mode == AuthMode::Username {
        let user = settings.auth.username.as_deref().unwrap_or("").trim();
        let pass = settings.auth.password.as_deref().unwrap_or("").trim();
        if user.is_empty() || pass.is_empty() {
            return Err(anyhow!("Invalid OPC auth config: username mode requires non-empty auth.username and auth.password"));
        }
    }

    // 4. Define Nodes
    let namespace = settings.opcua.namespace_index;
    
    let mut read_nodes = vec![
        NodeId::new(namespace, NODE_REFLUX_SP),
        NodeId::new(namespace, NODE_STEAM_SP),
        NodeId::new(namespace, NODE_FEED_DV),
        NodeId::new(namespace, NODE_CSTR_COOLING_SP),
        NodeId::new(namespace, NODE_CSTR_FEED_DV),
    ];
    let acquisition_nodes = vec![
        NodeId::new(2, settings.acquisition.debutanizer.as_str()),
        NodeId::new(2, settings.acquisition.cstr.as_str()),
        NodeId::new(2, settings.acquisition.pass_balancing.as_str()),
    ];
    let mut acquisition_sequence = opc_interface::AcquisitionSequence::default();
    let write_reflux_applied = NodeId::new(namespace, "Reflux:AppliedSP");
    let write_steam_applied = NodeId::new(namespace, "Steam:AppliedSP");
    let write_cstr_applied = NodeId::new(namespace, "CSTR_Cooling:AppliedSP");
    let write_feed_acquired = NodeId::new(namespace, "Feed_Flow:AcquiredPV");
    let write_cstr_feed_acquired = NodeId::new(namespace, "CSTR_Feed:AcquiredPV");

    let write_deb_top = NodeId::new(namespace, NODE_TOP_C4_CV);
    let write_deb_btm = NodeId::new(namespace, NODE_BTM_TEMP_CV);
    let write_reflux_pv = NodeId::new(namespace, NODE_REFLUX_PV);
    let write_reflux_op = NodeId::new(namespace, NODE_REFLUX_OP);
    let write_steam_pv = NodeId::new(namespace, NODE_STEAM_PV);
    let write_steam_op = NodeId::new(namespace, NODE_STEAM_OP);
    
    let write_cstr_temp = NodeId::new(namespace, NODE_CSTR_TEMP_CV);
    let write_cstr_conc = NodeId::new(namespace, NODE_CSTR_CONC_CV);
    let write_cstr_cool_pv = NodeId::new(namespace, NODE_CSTR_COOLING_PV);
    let write_cstr_cool_op = NodeId::new(namespace, NODE_CSTR_COOLING_OP);

    let node_flow_master_op = NodeId::new(namespace, NODE_FLOW_MASTER_OP);
    let write_flow_master_pv = NodeId::new(namespace, NODE_FLOW_MASTER_PV);
    let node_coke_enable = NodeId::new(namespace, NODE_COKE_ENABLE);
    let node_disturbance_enable = NodeId::new(namespace, NODE_DISTURBANCE_ENABLE);
    let write_disturbance_cv_pv = NodeId::new(namespace, NODE_DISTURBANCE_CV_PV);
    let node_disturbance_cv_target = NodeId::new(namespace, NODE_DISTURBANCE_CV_TARGET);
    let write_sum_bias = NodeId::new(namespace, NODE_SUM_BIAS);
    let write_sum_bias_cv_pv = NodeId::new(namespace, NODE_SUM_BIAS_CV_PV);

    let fc_bias_sp_nodes: Vec<NodeId> = (1..=6)
        .map(|i| NodeId::new(namespace, format!("FC{}_Bias:SP", i)))
        .collect();
    let fc_bias_pv_nodes: Vec<NodeId> = (1..=6)
        .map(|i| NodeId::new(namespace, format!("FC{}_Bias:PV", i)))
        .collect();
    let fc_bias_op_nodes: Vec<NodeId> = (1..=6)
        .map(|i| NodeId::new(namespace, format!("FC{}_Bias:OP", i)))
        .collect();
    let fc_bias_applied_nodes: Vec<NodeId> = (1..=6)
        .map(|i| NodeId::new(namespace, format!("FC{}_Bias:AppliedSP", i)))
        .collect();
    let fc_op_nodes: Vec<NodeId> = (1..=6)
        .map(|i| NodeId::new(namespace, format!("FC{}:OP", i)))
        .collect();
    let fc_pv_nodes: Vec<NodeId> = (1..=6)
        .map(|i| NodeId::new(namespace, format!("FC{}:PV", i)))
        .collect();
    let t_pv_nodes: Vec<NodeId> = (1..=6)
        .map(|i| NodeId::new(namespace, format!("T{}:PV", i)))
        .collect();
    let tdev_pv_nodes: Vec<NodeId> = (1..=6)
        .map(|i| NodeId::new(namespace, format!("T{}_Dev:PV", i)))
        .collect();
    let pass_flow_nodes: Vec<NodeId> = (1..=6)
        .map(|i| NodeId::new(namespace, format!("Pass{}:Flow", i)))
        .collect();
    let coke_offset_nodes: Vec<NodeId> = (1..=6)
        .map(|i| NodeId::new(namespace, format!("Pass{}:CokeOffset", i)))
        .collect();
    let coke_ramp_nodes: Vec<NodeId> = (1..=6)
        .map(|i| NodeId::new(namespace, format!("Pass{}:CokeRampPerMin", i)))
        .collect();

    // One checked acquisition reads all real simulator inputs after setting odd.
    read_nodes.push(node_flow_master_op.clone());
    read_nodes.extend(fc_bias_sp_nodes.iter().cloned());
    read_nodes.push(node_coke_enable.clone());
    read_nodes.push(node_disturbance_cv_target.clone());
    read_nodes.extend(coke_offset_nodes.iter().cloned());
    read_nodes.extend(coke_ramp_nodes.iter().cloned());

    // 5. Initialize Physics Engines
    let mut plant_deb = DebutanizerModel::new(ColumnConfig {
        num_stages: settings.debutanizer.num_stages,
        feed_stage: settings.debutanizer.feed_stage,
        relative_volatility: settings.debutanizer.relative_volatility,
        hold_up_molar: settings.debutanizer.hold_up_molar,       
        dt_seconds: settings.debutanizer.dt_seconds,          
    });

    let mut plant_cstr = CSTRModel::new();
    let mut plant_pass = PassBalancingModel::new(1.0, PassBalancingConfig::default());

    println!("Physics Engines Loaded. Waiting for OPC UA...");

    // Parse security settings
    let security_policy = match settings.opcua.security_policy.as_str() {
        "None" => SecurityPolicy::None,
        "Basic128Rsa15" => SecurityPolicy::Basic128Rsa15,
        "Basic256" => SecurityPolicy::Basic256,
        "Basic256Sha256" => SecurityPolicy::Basic256Sha256,
        "Aes128Sha256RsaOaep" => SecurityPolicy::Aes128Sha256RsaOaep,
        "Aes256Sha256RsaPss" => SecurityPolicy::Aes256Sha256RsaPss,
        _ => {
            println!("Unknown security policy, defaulting to Basic256Sha256");
            SecurityPolicy::Basic256Sha256
        }
    };
    
    let message_mode = match settings.opcua.message_mode.as_str() {
        "None" => MessageSecurityMode::None,
        "Sign" => MessageSecurityMode::Sign,
        "SignAndEncrypt" => MessageSecurityMode::SignAndEncrypt,
        _ => {
            println!("Unknown message mode, defaulting to SignAndEncrypt");
            MessageSecurityMode::SignAndEncrypt
        }
    };

    // 6. Connection Loop
    loop {
        println!("Connecting to {}...", settings.opcua.endpoint_url);
        let (identity, token_type) = match settings.auth.mode {
            AuthMode::Username => (
                IdentityToken::UserName(
                    settings.auth.username.clone().unwrap_or_default(),
                    settings.auth.password.clone().unwrap_or_default().into(),
                ),
                UserTokenType::UserName,
            ),
            AuthMode::X509 => (
                IdentityToken::X509(Box::new(x509.clone()), Box::new(private_key.clone())),
                UserTokenType::Certificate,
            ),
        };
        
        let connect_result = tokio::time::timeout(
            Duration::from_secs(15),
            client_builder.connect_to_matching_endpoint(
                (
                    settings.opcua.endpoint_url.as_str(),
                    security_policy.to_str(),
                    message_mode,
                    UserTokenPolicy { token_type, ..Default::default() },
                ),
                identity,
            ),
        ).await;

        let session = match connect_result {
            Ok(Ok((s, _h))) => {
                let _ = _h.spawn();
                s
            }
            Ok(Err(e)) => {
                println!("Connect failed: {}. Retrying in {}s...", e, settings.runtime.reconnect_delay_sec);
                tokio::time::sleep(Duration::from_secs(settings.runtime.reconnect_delay_sec)).await;
                continue;
            }
            Err(_) => {
                println!("Connect timed out after 15s. Retrying in {}s...", settings.runtime.reconnect_delay_sec);
                tokio::time::sleep(Duration::from_secs(settings.runtime.reconnect_delay_sec)).await;
                continue;
            }
        };

        let connected = tokio::time::timeout(Duration::from_secs(10), session.wait_for_connection())
            .await
            .unwrap_or(false);
        if !connected {
            println!("Connection dropped before session activation. Retrying in {}s...", settings.runtime.reconnect_delay_sec);
            tokio::time::sleep(Duration::from_secs(settings.runtime.reconnect_delay_sec)).await;
            continue;
        }

        println!("Connected! Running Simulation.");
        
        // Wait for session to be fully ready for writes
        println!("Waiting for session to stabilize...");
        tokio::time::sleep(Duration::from_secs(3)).await;
        
        // No startup SP, PID mode, ModeTarget, target, tuning or limit writes.
        // This simulator has ideal SP tracking for Reflux/Steam/CSTR_Cooling and
        // additive pass bias commands; its physical equations are not an actuator proof.
        let mut frame = Vec::with_capacity(68);
        loop {
            let observed = opc_interface::read_bulk(&session, &acquisition_nodes).await
                .context("Required AcquisitionSequence nodes unreadable; publication stopped")?;
            let (odd, even) = acquisition_sequence.reserve(&observed)?;
            opc_interface::write_sequence(&session, &acquisition_nodes, odd).await
                .context("Cannot mark acquisition in progress; publication stopped")?;

            // Every error below is fatal, leaving the in-progress counters odd.
            // Never reconnect and silently claim a failed/unknown frame succeeded.
            let vals = opc_interface::read_bulk(&session, &read_nodes).await
                .context("Process acquisition failed; counters remain odd")?;
            let reflux_m3h = vals[0].max(0.0);
            let steam_m3h = vals[1].max(0.0);
            let feed_m3h = vals[2].max(0.0);
            let cool_temp = vals[3].max(250.0);
            let feed_flow = vals[4].max(0.0);
            let flow_master_op = vals[5];
            let mut bias_sp = [0.0; 6];
            bias_sp.copy_from_slice(&vals[6..12]);
            let coke_enable = vals[12] >= 0.5;
            let disturbance_enable = vals[13].clamp(0.0, 1.0) >= 0.5;
            let mut coke_offset = [0.0; 6];
            coke_offset.copy_from_slice(&vals[14..20]);
            let mut coke_ramp_per_min = [0.0; 6];
            coke_ramp_per_min.copy_from_slice(&vals[20..26]);

            let deb_inputs = ModelInputs {
                reflux_l: (reflux_m3h * DENSITY) / MW,
                boilup_v: (steam_m3h * DENSITY) / MW,
                feed_f: (feed_m3h * DENSITY) / MW,
                feed_z: 0.5,
            };
            let cstr_inputs = CSTRInputs {
                flow_f: feed_flow,
                cool_temp,
                ca_feed: 10.0,
                t_feed: 300.0,
            };
            let pass_inputs = PassBalancingInputs {
                flow_master_op,
                bias_sp,
                coke_enable: disturbance_enable || coke_enable,
                coke_offset,
                coke_ramp_per_min,
            };

            // Retain the existing physics step counts, clamps and model parameters.
            let mut deb_out = plant_deb.step(&deb_inputs);
            let mut cstr_out = plant_cstr.step(&cstr_inputs);
            let mut pass_out = plant_pass.step(&pass_inputs);
            for _ in 0..(settings.runtime.speed_multiplier - 1) {
                deb_out = plant_deb.step(&deb_inputs);
                cstr_out = plant_cstr.step(&cstr_inputs);
                pass_out = plant_pass.step(&pass_inputs);
            }

            // Full CV / effective-MV / sampled-DV frame, plus simulator telemetry.
            // AppliedSP and AcquiredPV are producer-owned, unlike command SP / feed PV.
            frame.clear();
            frame.extend([
                (&write_deb_top, Variant::Double(deb_out.top_c4_frac)),
                (&write_deb_btm, Variant::Double(deb_out.bottom_temp_deg_c)),
                (&write_reflux_pv, Variant::Double(reflux_m3h)),
                (&write_reflux_op, Variant::Double(reflux_m3h)),
                (&write_reflux_applied, Variant::Double(reflux_m3h)),
                (&write_steam_pv, Variant::Double(steam_m3h)),
                (&write_steam_op, Variant::Double(steam_m3h)),
                (&write_steam_applied, Variant::Double(steam_m3h)),
                (&write_feed_acquired, Variant::Double(feed_m3h)),
                (&write_cstr_temp, Variant::Double(cstr_out.t)),
                (&write_cstr_conc, Variant::Double(cstr_out.ca)),
                (&write_cstr_cool_pv, Variant::Double(cool_temp)),
                (&write_cstr_cool_op, Variant::Double(cool_temp)),
                (&write_cstr_applied, Variant::Double(cool_temp)),
                (&write_cstr_feed_acquired, Variant::Double(feed_flow)),
                (&write_flow_master_pv, Variant::Double(pass_out.total_flow)),
                (&node_disturbance_enable, Variant::Boolean(disturbance_enable)),
                (&write_disturbance_cv_pv, Variant::Double(if disturbance_enable { 1.0 } else { 0.0 })),
                (&write_sum_bias, Variant::Double(pass_out.sum_bias)),
                (&write_sum_bias_cv_pv, Variant::Double(pass_out.sum_bias)),
            ]);
            for i in 0..6 {
                // Preserve bias-SP coordinates after the existing valve clamp;
                // absolute valve OP is not the model's manipulated variable.
                let applied_bias = pass_out.valve_op[i] - flow_master_op;
                frame.extend([
                    (&t_pv_nodes[i], Variant::Double(pass_out.temperatures[i])),
                    (&tdev_pv_nodes[i], Variant::Double(pass_out.temp_deviation[i])),
                    (&fc_op_nodes[i], Variant::Double(pass_out.valve_op[i])),
                    (&fc_pv_nodes[i], Variant::Double(pass_out.pass_flow[i])),
                    (&fc_bias_pv_nodes[i], Variant::Double(applied_bias)),
                    (&fc_bias_op_nodes[i], Variant::Double(bias_sp[i])),
                    (&fc_bias_applied_nodes[i], Variant::Double(applied_bias)),
                    (&pass_flow_nodes[i], Variant::Double(pass_out.pass_flow[i])),
                ]);
            }
            opc_interface::write_bulk(&session, &frame).await
                .context("Process frame write failed/unknown; counters remain odd; publication stopped")?;
            opc_interface::commit_sequence(&session, &acquisition_nodes, even).await?;

            println!(
                "Tick (+{}s, acquisition {}): [DEB] C4:{:.2}% BtmT:{:.1}C | [CSTR] T:{:.1}K Ca:{:.2}kmol/m3 | [PASS] TDev1:{:+.2} TDev6:{:+.2} SumBias:{:+.2}",
                settings.runtime.speed_multiplier, even,
                deb_out.top_c4_frac * 100.0, deb_out.bottom_temp_deg_c,
                cstr_out.t, cstr_out.ca,
                pass_out.temp_deviation[0], pass_out.temp_deviation[5], pass_out.sum_bias
            );
            tokio::time::sleep(Duration::from_millis(settings.runtime.cycle_time_ms)).await;
        }
    }
}
