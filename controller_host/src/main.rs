mod artifacts;
mod runtime;

use axum::{
    extract::{Path, Query, State, Request},
    routing::{get, post},
    Json, Router,
    http::StatusCode,
    response::IntoResponse,
    middleware::{self, Next},
};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, net::SocketAddr, path::PathBuf, sync::{Arc, Mutex}, fs::File};
use tokio::{fs, process::Command};
use std::process::Stdio;
use tracing::{info, error, warn};
use serde_json::json;
use chrono::{Local, DateTime, Utc};

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    /// Port to listen on
    #[arg(short, long, default_value_t = 8080)]
    port: u16,

    /// IP address to bind; use 127.0.0.1 for isolated local validation.
    #[arg(long, default_value = "0.0.0.0")]
    bind_address: std::net::IpAddr,

    /// Directory to store model files
    #[arg(short, long, default_value = "./models")]
    model_dir: String,

    /// Path to apc_engine binary
    #[arg(long)]
    engine_bin: Option<String>,

    /// API Key for authentication
    #[arg(long, env = "HOST_API_KEY", default_value = "secret-deploy-key")]
    api_key: String,

    /// PKI Directory (Authentication)
    #[arg(long, default_value = "./pki")]
    pki_dir: String,

    /// Path to persisted OPC client defaults env file
    #[arg(long, default_value = "./config/opc_client.env")]
    opc_defaults_path: String,
    /// Private local authority state; must be mode 0700 and have a short absolute path.
    #[arg(long, default_value = "./runtime")]
    runtime_dir: PathBuf,
}

#[derive(Clone)]
struct AppState {
    model_dir: PathBuf,
    engine_bin: PathBuf,
    pki_dir: PathBuf,
    opc_defaults_path: PathBuf,
    runtime_dir: PathBuf,
    api_key: String,
    opc_client_defaults: Arc<Mutex<OpcClientDefaults>>,
    // Each monitor owns its child; stop requests never signal a recycled PID.
    processes: Arc<Mutex<HashMap<String, ProcessStatus>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpcClientDefaults {
    opc_url: String,
    auth_mode: String,
    username: Option<String>,
    password: Option<String>,
    security_policy: String,
    message_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
enum ProcessState {
    Stopped,
    Running(u32), // PID
    Failed,
}

type StopRequest = tokio::sync::oneshot::Sender<std::io::Result<()>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProcessStatus {
    id: String,
    active_model: Option<String>,
    model_sha256: Option<String>,
    control_version_id: Option<String>,
    state: ProcessState,
    last_error: Option<String>,
    started_at: Option<DateTime<Utc>>,
    stopped_at: Option<DateTime<Utc>>,
    last_log_path: Option<String>,
    #[serde(skip)]
    stop_sender: Option<tokio::sync::mpsc::Sender<StopRequest>>,
    #[serde(skip)]
    authority: Option<Arc<runtime::Launch>>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    // Determine Engine Binary Path
    let engine_bin = if let Some(path) = args.engine_bin {
        PathBuf::from(path)
    } else {
        // Smart fallback: Check CWD first (Production), then Dev path
        let exe_name = format!("authrity-apc-engine{}", std::env::consts::EXE_SUFFIX);
        let cwd_engine = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(&exe_name);
            
        if cwd_engine.exists() {
            info!("Found local engine binary: {:?}", cwd_engine);
            cwd_engine
        } else {
            // Fallback for development
            let dev_path = PathBuf::from("../apc_engine/target/debug").join(&exe_name);
            warn!("Local engine not found. Using dev path: {:?}", dev_path);
            dev_path
        }
    };

    if !engine_bin.exists() {
        error!("❌ APC Engine binary not found at: {:?}. Please invoke with --engine-bin", engine_bin);
        // We continue in case the user plans to upload it later? Or just crash?
        // Better to crash or warn heavily.
    }

    // Ensure model dir exists
    let model_dir = PathBuf::from(&args.model_dir);
    if !model_dir.exists() {
        fs::create_dir_all(&model_dir).await.expect("Failed to create model dir");
    }

    // Determine PKI Directory
    let pki_dir = PathBuf::from(&args.pki_dir);
    let final_pki_dir = if pki_dir.exists() {
        pki_dir
    } else {
        // Fallback for development
        let dev_pki = PathBuf::from("../apc_engine/pki");
        if dev_pki.exists() {
            info!("Found dev PKI: {:?}", dev_pki);
            dev_pki
        } else {
            pki_dir // Keep default
        }
    };
    
    // Canonicalize to absolute path to ensure child process finds it regardless of CWD
    let abs_pki = std::fs::canonicalize(&final_pki_dir).unwrap_or(final_pki_dir);
    let opc_defaults_path = PathBuf::from(&args.opc_defaults_path);
    let initial_defaults = load_opc_client_defaults(&opc_defaults_path).unwrap_or_else(|| OpcClientDefaults {
        opc_url: "opc.tcp://127.0.0.1:4855".to_string(),
        auth_mode: "username".to_string(),
        username: None,
        password: None,
        security_policy: "Basic256Sha256".to_string(),
        message_mode: "SignAndEncrypt".to_string(),
    });

    let runtime_dir=runtime::private_root(&args.runtime_dir).expect("Invalid private native authority directory");
    let shared_state = AppState {
        model_dir,
        engine_bin,
        pki_dir: abs_pki,
        opc_defaults_path,
        runtime_dir,
        api_key: args.api_key.clone(),
        opc_client_defaults: Arc::new(Mutex::new(initial_defaults)),
        processes: Arc::new(Mutex::new(HashMap::new())),
    };

    let app = Router::new()
        .route("/api/controllers", get(list_controllers))
        .route("/api/opc-client/defaults", get(get_opc_client_defaults).post(set_opc_client_defaults))
        .route("/api/controllers/:id/start", post(start_controller))
        .route("/api/controllers/:id/runtime", get(runtime::status).post(runtime::command))
        .route("/api/controllers/:id/runtime/operations/:operation_id",get(runtime::operation))
        .route("/api/controllers/:id/logs/tail", get(get_log_tail))
        .route("/api/controllers/:id/config", get(get_model_config)) // <--- Added: Access to Single Source of Truth for Physics
        .route("/api/controllers/:id/models/:filename", get(get_model_file)) // <--- Added: Fetch specific model file
        .route("/api/deploy", post(deploy_model))
        .route("/api/control-versions", post(install_control_version))
        .layer(middleware::from_fn_with_state(shared_state.clone(), auth_middleware))
        .route("/api/health", get(health_check)) // Health check bypasses auth
        .with_state(shared_state);

    let addr = SocketAddr::new(args.bind_address, args.port);
    info!("🚀 Supervisor listening on {}", addr);
    
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn auth_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> impl IntoResponse {
    let auth_header = req.headers().get("x-api-key");
    
    match auth_header {
        Some(header) if header == state.api_key.as_str() => {
            next.run(req).await
        }
        _ => {
            warn!("⛔ Unauthorized access attempt");
            (StatusCode::UNAUTHORIZED, "Invalid API Key").into_response()
        }
    }
}

async fn health_check() -> &'static str {
    "OK"
}

#[derive(Deserialize)]
struct LogTailQuery {
    lines: Option<usize>,
}

async fn get_log_tail(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<LogTailQuery>,
) -> impl IntoResponse {
    let log_path = {
        let processes = state.processes.lock().unwrap();
        processes.get(&id).and_then(|p| p.last_log_path.clone())
    };

    let Some(path_str) = log_path else {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({
            "id": id,
            "lines": [],
            "error": "No log file recorded for this controller"
        }))).into_response();
    };

    let n = query.lines.unwrap_or(80).min(500);

    match tokio::fs::read_to_string(&path_str).await {
        Ok(contents) => {
            let lines: Vec<&str> = contents.lines().collect();
            let tail: Vec<&str> = lines.iter().rev().take(n).rev().cloned().collect();
            Json(serde_json::json!({
                "id": id,
                "log_path": path_str,
                "lines": tail,
            })).into_response()
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({
                "id": id,
                "log_path": path_str,
                "lines": [],
                "error": format!("Failed to read log: {}", e)
            }))).into_response()
        }
    }
}

// DTOs
#[derive(Serialize)]
struct ControllerSummary {
    id: String,
    models: Vec<String>,
    active_model: Option<String>,
    model_sha256: Option<String>,
    control_version_id: Option<String>,
    state: ProcessState,
    last_error: Option<String>,
    started_at: Option<DateTime<Utc>>,
    stopped_at: Option<DateTime<Utc>>,
    last_log_path: Option<String>,
}

/// List all available model folders and their running state
async fn list_controllers(State(state): State<AppState>) -> Json<Vec<ControllerSummary>> {
    let available = match artifacts::list(&state.model_dir) {
        Ok(available) => available,
        Err(error) => {
            error!("Cannot list controller models: {error:#}");
            return Json(Vec::new());
        }
    };
    let processes = state.processes.lock().unwrap();
    Json(available.into_iter().map(|(id, models)| {
        let status = processes.get(&id);
        ControllerSummary {
            id: id.clone(),
            models,
            active_model: status.and_then(|p| p.active_model.clone()),
            model_sha256: status.and_then(|p| p.model_sha256.clone()),
            control_version_id: status.and_then(|p| p.control_version_id.clone()),
            state: status.map(|p| p.state.clone()).unwrap_or(ProcessState::Stopped),
            last_error: status.and_then(|p| p.last_error.clone()),
            started_at: status.and_then(|p| p.started_at),
            stopped_at: status.and_then(|p| p.stopped_at),
            last_log_path: status.and_then(|p| p.last_log_path.clone()),
        }
    }).collect())
}

#[derive(Deserialize)]
struct StartRequest {
    operation_id: String,
    expires_at:DateTime<Utc>,
    commissioning_profile: serde_json::Value,
    approval:Option<serde_json::Value>,
    #[serde(default)]
    recover_monitor:bool,
    model_filename: String, // REQUIRED now
    opc_url: Option<String>,
    security_policy: Option<String>,
    message_mode: Option<String>,
    auth_mode: Option<String>,
    username: Option<String>,
    password: Option<String>,
    acquisition_sequence_node: Option<String>,
    max_acquisition_age_secs: Option<f64>,
    max_read_to_command_secs: Option<f64>,
    command_confirmation_secs: Option<f64>,
    command_confirmation_tolerance: Option<f64>,
    monitor_window_samples: Option<usize>,
    live_tuning: Option<bool>,
}

#[derive(Deserialize)]
struct SetOpcClientDefaultsRequest {
    opc_url: Option<String>,
    security_policy: Option<String>,
    message_mode: Option<String>,
    auth_mode: Option<String>,
    username: Option<String>,
    password: Option<String>,
}

#[derive(Serialize)]
struct OpcClientDefaultsResponse {
    opc_url: String,
    security_policy: String,
    message_mode: String,
    auth_mode: String,
    username: Option<String>,
}

fn validate_auth_payload(auth_mode: &str, username: &Option<String>, password: &Option<String>) -> Result<(), String> {
    let auth_mode_normalized = auth_mode.to_ascii_lowercase();
    if auth_mode_normalized != "username" && auth_mode_normalized != "x509" && auth_mode_normalized != "anonymous" {
        return Err("auth_mode must be 'username', 'x509', or 'anonymous'".to_string());
    }

    if auth_mode_normalized == "username" {
        let user_ok = username.as_deref().map(str::trim).is_some_and(|u| !u.is_empty());
        let pass_ok = password.as_deref().map(str::trim).is_some_and(|p| !p.is_empty());
        if !user_ok || !pass_ok {
            return Err("username auth_mode requires non-empty username and password".to_string());
        }
    }

    Ok(())
}

async fn get_opc_client_defaults(State(state): State<AppState>) -> impl IntoResponse {
    let defaults = state.opc_client_defaults.lock().unwrap().clone();
    Json(OpcClientDefaultsResponse {
        opc_url: defaults.opc_url,
        security_policy: defaults.security_policy,
        message_mode: defaults.message_mode,
        auth_mode: defaults.auth_mode,
        username: defaults.username,
    })
}

async fn set_opc_client_defaults(
    State(state): State<AppState>,
    Json(payload): Json<SetOpcClientDefaultsRequest>,
) -> impl IntoResponse {
    let mut defaults = state.opc_client_defaults.lock().unwrap();

    let next_auth_mode = payload
        .auth_mode
        .clone()
        .unwrap_or_else(|| defaults.auth_mode.clone());
    let next_username = payload.username.clone().or_else(|| defaults.username.clone());
    let next_password = payload.password.clone().or_else(|| defaults.password.clone());

    if let Err(msg) = validate_auth_payload(&next_auth_mode, &next_username, &next_password) {
        return (StatusCode::BAD_REQUEST, msg).into_response();
    }

    if let Some(opc_url) = payload.opc_url {
        if !opc_url.trim().is_empty() {
            defaults.opc_url = opc_url;
        }
    }
    if let Some(policy) = payload.security_policy {
        if !policy.trim().is_empty() {
            defaults.security_policy = policy;
        }
    }
    if let Some(mode) = payload.message_mode {
        if !mode.trim().is_empty() {
            defaults.message_mode = mode;
        }
    }

    defaults.auth_mode = next_auth_mode;
    defaults.username = next_username;
    defaults.password = next_password;

    let snapshot = defaults.clone();
    drop(defaults);

    if let Err(err) = persist_opc_client_defaults(&state.opc_defaults_path, &snapshot) {
        error!("Failed to persist controller_host OPC defaults: {}", err);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to persist OPC defaults: {}", err),
        )
            .into_response();
    }

    info!("Updated controller_host OPC client defaults");
    StatusCode::OK.into_response()
}

fn reserve_start<'a>(
    processes: &'a Mutex<HashMap<String, ProcessStatus>>,
    id: &str,
) -> Result<std::sync::MutexGuard<'a, HashMap<String, ProcessStatus>>, &'static str> {
    let guard = processes.lock().unwrap();
    if guard.get(id).is_some_and(|status| matches!(status.state, ProcessState::Running(_))) {
        return Err("Controller is already running");
    }
    Ok(guard)
}

async fn start_controller(
    State(state): State<AppState>,
    Path(id): Path<String>, // 'id' is the folder name
    Json(payload): Json<StartRequest>,
) ->  impl IntoResponse {
    // Validate exact bytes and any immutable receipt before reserving/spawning.
    if payload.expires_at<=Utc::now()||payload.expires_at>Utc::now()+chrono::Duration::seconds(300) {
        return (StatusCode::CONFLICT,"Commissioned start intent expired or exceeds its authorization window").into_response();
    }
    let loaded=if let Some(proof)=payload.approval.as_ref(){artifacts::load_approved(&state.model_dir,&id,&payload.model_filename,proof)}
        else{artifacts::load(&state.model_dir,&id,&payload.model_filename)};
    let model = match loaded {
        Ok(model) => model,
        Err(error) => return (StatusCode::BAD_REQUEST, format!("{error:#}")).into_response(),
    };
    let live_tuning = payload.live_tuning.unwrap_or(false);
    if live_tuning {
        return (StatusCode::BAD_REQUEST, "Commissioned controllers cannot enable manual live tuning").into_response();
    }
    if payload.acquisition_sequence_node.as_deref().is_none_or(str::is_empty)
        ||payload.acquisition_sequence_node.as_deref()!=payload.commissioning_profile["acquisition_sequence_node"].as_str() {
        return (StatusCode::BAD_REQUEST,"Acquisition sequence must match the exact commissioned producer contract").into_response();
    }
    let model_path = &model.path;
    // There are no await points below. Holding this guard through spawn + insertion
    // atomically reserves the slot, and every early return releases the reservation.
    let mut processes = match reserve_start(&state.processes, &id) {
        Ok(guard) => guard,
        Err(message) => return (StatusCode::CONFLICT, message).into_response(),
    };

    info!("Starting controller {} with model {}...", id, payload.model_filename);
    
    // Determine OPC client settings (payload overrides controller_host defaults)
    let mut defaults = state.opc_client_defaults.lock().unwrap().clone();
    if let Some(opc_url) = payload.opc_url.clone() {
        if !opc_url.trim().is_empty() {
            defaults.opc_url = opc_url;
        }
    }
    if let Some(policy) = payload.security_policy.clone() {
        if !policy.trim().is_empty() {
            defaults.security_policy = policy;
        }
    }
    if let Some(mode) = payload.message_mode.clone() {
        if !mode.trim().is_empty() {
            defaults.message_mode = mode;
        }
    }
    if let Some(auth_mode) = payload.auth_mode.clone() {
        defaults.auth_mode = auth_mode;
    }
    if payload.username.is_some() {
        defaults.username = payload.username.clone();
    }
    if payload.password.is_some() {
        defaults.password = payload.password.clone();
    }

    if let Err(msg) = validate_auth_payload(&defaults.auth_mode, &defaults.username, &defaults.password) {
        return (StatusCode::BAD_REQUEST, msg).into_response();
    }

    let opc_url = defaults.opc_url.clone();
    let auth_mode = defaults.auth_mode.clone();
    let security_policy = defaults.security_policy.clone();
    let message_mode = defaults.message_mode.clone();
    let username = defaults.username.clone();
    let password = defaults.password.clone();
    match runtime::register_start(&state.runtime_dir,&id,&payload.operation_id,&payload.commissioning_profile,&model.model_sha256,payload.approval.as_ref(),payload.recover_monitor) {
        Ok(Some(receipt))=>return Json(receipt).into_response(),
        Ok(None)=>{},
        Err(error)=>return (StatusCode::CONFLICT,format!("{error:#}")).into_response(),
    }
    let authority=match runtime::Launch::prepare(&state.runtime_dir,&id,&payload.commissioning_profile,&model,&opc_url,payload.approval.as_ref(),payload.recover_monitor) {
        Ok(launch)=>launch,
        Err(error)=>return (StatusCode::CONFLICT,format!("{error:#}")).into_response(),
    };
    let auth_mode_normalized = auth_mode.to_ascii_lowercase();

    // Persist resolved settings as the new default for subsequent starts
    {
        let mut default_guard = state.opc_client_defaults.lock().unwrap();
        *default_guard = defaults;
        if let Err(err) = persist_opc_client_defaults(&state.opc_defaults_path, &default_guard) {
            warn!("Failed to persist resolved controller defaults after start: {}", err);
        }
    }

    info!("   -> Targeting OPC Server: {}", opc_url);
    info!("   -> OPC Auth Mode: {}", auth_mode);
    info!("   -> OPC Security Policy: {}", security_policy);
    info!("   -> OPC Message Mode: {}", message_mode);
    info!("   -> Using PKI: {:?}", state.pki_dir);

    // Prepare Log File
    let log_dir = PathBuf::from("./logs");
    if !log_dir.exists() {
        if let Err(e) = std::fs::create_dir_all(&log_dir) {
             error!("Failed to create log dir: {}", e);
             return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to create log dir").into_response();
        }
    }

    let timestamp = Local::now().format("%Y%m%d_%H%M%S").to_string();
    let log_filename = format!("{}_{}.log", id, timestamp);
    let log_path = log_dir.join(&log_filename);
    
    info!("   -> Logging to: {:?}", log_path);

    let log_file = match File::create(&log_path) {
        Ok(f) => f,
        Err(e) => {
            error!("Failed to create log file: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to create log file").into_response();
        }
    };
    
    // Clone file handle for stderr (stdout and stderr go to same file)
    let log_file_err = match log_file.try_clone() {
        Ok(f) => f,
        Err(e) => {
             error!("Failed to clone log file handle: {}", e);
             return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to setup logging").into_response();
        }
    };

    // Spawn Process
    let mut command = Command::new(&state.engine_bin);
    command
        .arg("--model")
        .arg(&model_path)
        .arg("--expected-model-sha256")
        .arg(&model.model_sha256)
        .arg("--model-output-dir")
        .arg(state.model_dir.join(&id))
        .arg("--opc")
        .arg(&opc_url)
        .arg("--pki")
        .arg(&state.pki_dir)
        .arg("--auth-mode")
        .arg(&auth_mode)
        .arg("--security-policy")
        .arg(&security_policy)
        .arg("--message-mode")
        .arg(&message_mode)
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file_err));
    command.arg("--authority-socket").arg(&authority.socket)
        .arg("--commissioning-profile").arg(&authority.profile_path);
    if let Some(path)=&authority.startup_approval_path{command.arg("--startup-approval").arg(path);}
    if payload.recover_monitor {command.arg("--recover-monitor");}

    if live_tuning {
        command.arg("--live-tuning");
    }
    if let Some(node) = &payload.acquisition_sequence_node {
        command.arg("--acquisition-sequence-node").arg(node);
    }
    if let Some(samples) = payload.monitor_window_samples {
        command.arg("--monitor-window-samples").arg(samples.to_string());
    }
    for (flag, value) in [
        ("--max-acquisition-age-secs", payload.max_acquisition_age_secs),
        ("--max-read-to-command-secs", payload.max_read_to_command_secs),
        ("--command-confirmation-secs", payload.command_confirmation_secs),
        ("--command-confirmation-tolerance", payload.command_confirmation_tolerance),
    ] {
        if let Some(value) = value {
            command.arg(flag).arg(value.to_string());
        }
    }

    let child_res = if auth_mode_normalized == "username" {
        command
            .arg("--username")
            .arg(username.as_deref().unwrap_or_default())
            .arg("--password")
            .arg(password.as_deref().unwrap_or_default())
            .spawn()
    } else {
        command.spawn()
    };

    match child_res {
        Ok(mut child) => {
            match child.try_wait() {
                Ok(Some(status)) => {
                    error!(
                        "Controller process exited immediately with status {}. See log: {:?}",
                        status,
                        log_path
                    );
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!(
                            "Controller failed to start (exit: {}). Check log: {}",
                            status,
                            log_path.display()
                        ),
                    )
                        .into_response();
                }
                Ok(None) => {}
                Err(e) => {
                    warn!("Could not query child process status after spawn: {}", e);
                }
            }

            // tokio::process::Child::id() returns Option<u32>; unwrap_or(0) is safe here
            // since a successfully spawned process will always have a PID at this point
            let pid = child.id().unwrap_or(0);
            info!("Started controller {} with PID {}", id, pid);
            let log_path_str = log_path.to_string_lossy().to_string();
            processes.insert(id.clone(), ProcessStatus {
                id: id.clone(),
                active_model: Some(payload.model_filename.clone()),
                model_sha256: Some(model.model_sha256.clone()),
                control_version_id: model.control_version_id.clone(),
                state: ProcessState::Running(pid),
                last_error: None,
                started_at: Some(Utc::now()),
                stopped_at: None,
                last_log_path: Some(log_path_str),
                stop_sender: None,
                authority: Some(authority),
            });
            drop(processes);

            monitor_controller(child, state.processes.clone(), id.clone(), pid);

            let receipt=json!({"ok":true,"confirmed":false,"kind":"start","operation_id":payload.operation_id,
                "profile_hash":payload.commissioning_profile["profile_hash"],"pid":pid,
                "model_sha256":model.model_sha256,"control_version_id":model.control_version_id,
                "detail":"Process created; Monitor readiness/arming/application are separate"});
            match runtime::finish_start(&state.runtime_dir,&payload.operation_id,receipt) {
                Ok(receipt)=>Json(receipt).into_response(),
                Err(error)=>(StatusCode::INTERNAL_SERVER_ERROR,format!("Process exists but receipt durability failed: {error:#}; do not restart")).into_response(),
            }
        }
        Err(e) => {
            error!("Failed to spawn: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

fn monitor_controller(
    mut child: tokio::process::Child,
    processes_watcher: Arc<Mutex<HashMap<String, ProcessStatus>>>,
    watch_id: String,
    pid: u32,
) -> tokio::task::JoinHandle<()> {
    let (stop_sender, mut stop_requests) = tokio::sync::mpsc::channel::<StopRequest>(1);
    {
        let mut processes = processes_watcher.lock().unwrap();
        if let Some(status) = processes.get_mut(&watch_id) {
            if matches!(status.state, ProcessState::Running(p) if p == pid) {
                status.stop_sender = Some(stop_sender);
            }
        }
    }
    tokio::spawn(async move {
        let mut stopped_by_request = false;
        let exit_status = loop {
            tokio::select! {
                biased;
                // An already-observed crash remains a failure, even if a stop is queued.
                result = child.wait() => break result,
                Some(reply) = stop_requests.recv() => {
                    let result = if stopped_by_request { Ok(()) } else { child.start_kill() };
                    if result.is_ok() {
                        stopped_by_request = true;
                    }
                    let _ = reply.send(result);
                }
            }
        };
        let mut processes = processes_watcher.lock().unwrap();
        if let Some(status) = processes.get_mut(&watch_id) {
            // Only the owning child may release this controller's process slot.
            if matches!(status.state, ProcessState::Running(p) if p == pid) {
                if exit_status.is_ok() {
                    if let Some(authority)=&status.authority {authority.confirmed_exit();}
                }
                match exit_status {
                    Ok(s) if s.success() || stopped_by_request => {
                        info!("Controller {} stopped; operator request={}.", watch_id, stopped_by_request);
                        status.state = ProcessState::Stopped;
                        status.stopped_at = Some(Utc::now());
                        status.last_error = None;
                        status.stop_sender = None;
                    }
                    Ok(s) => {
                        let msg = format!("Exited unexpectedly: {s}");
                        warn!("Controller {} failed: {}", watch_id, msg);
                        status.state = ProcessState::Failed;
                        status.stopped_at = Some(Utc::now());
                        status.last_error = Some(msg);
                        status.stop_sender = None;
                    }
                    Err(e) => {
                        let msg = format!("Wait error: {}", e);
                        error!("Controller {} wait error: {}", watch_id, e);
                        // No confirmed exit: keep the slot occupied.
                        status.last_error = Some(msg);
                    }
                }
            }
        }
    })
}

fn load_opc_client_defaults(path: &PathBuf) -> Option<OpcClientDefaults> {
    let content = std::fs::read_to_string(path).ok()?;

    if content.trim_start().starts_with('{') {
        return serde_json::from_str::<OpcClientDefaults>(&content).ok();
    }

    let mut values: HashMap<String, String> = HashMap::new();
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some((key, value)) = line.split_once('=') {
            values.insert(key.trim().to_string(), value.trim().to_string());
        }
    }

    let opc_url = values.get("OPC_URL")?.clone();
    let auth_mode = values.get("AUTH_MODE")?.clone();
    let security_policy = values.get("SECURITY_POLICY")?.clone();
    let message_mode = values.get("MESSAGE_MODE")?.clone();

    let username = values
        .get("USERNAME")
        .filter(|value| !value.trim().is_empty())
        .cloned();
    let password = values
        .get("PASSWORD")
        .filter(|value| !value.trim().is_empty())
        .cloned();

    Some(OpcClientDefaults {
        opc_url,
        auth_mode,
        username,
        password,
        security_policy,
        message_mode,
    })
}

fn persist_opc_client_defaults(path: &PathBuf, defaults: &OpcClientDefaults) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create parent dir failed: {}", e))?;
    }

    let sanitize = |value: &str| value.replace('\n', "").replace('\r', "");
    let mut payload = String::new();
    payload.push_str("# Controller Host OPC client defaults\n");
    payload.push_str(&format!("OPC_URL={}\n", sanitize(&defaults.opc_url)));
    payload.push_str(&format!("AUTH_MODE={}\n", sanitize(&defaults.auth_mode)));
    payload.push_str(&format!("SECURITY_POLICY={}\n", sanitize(&defaults.security_policy)));
    payload.push_str(&format!("MESSAGE_MODE={}\n", sanitize(&defaults.message_mode)));
    payload.push_str(&format!(
        "USERNAME={}\n",
        defaults
            .username
            .as_deref()
            .map(sanitize)
            .unwrap_or_default()
    ));
    payload.push_str(&format!(
        "PASSWORD={}\n",
        defaults
            .password
            .as_deref()
            .map(sanitize)
            .unwrap_or_default()
    ));

    std::fs::write(path, payload).map_err(|e| format!("write defaults failed: {}", e))?;
    Ok(())
}

async fn stop_controller(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let stop_sender = {
        let processes = state.processes.lock().unwrap();
        match processes.get(&id) {
            Some(status) if matches!(status.state, ProcessState::Running(_)) => {
                match status.stop_sender.clone() {
                    Some(sender) => sender,
                    None => return (StatusCode::SERVICE_UNAVAILABLE, "Process monitor unavailable; slot remains reserved").into_response(),
                }
            }
            _ => return (StatusCode::NOT_FOUND, "Not running").into_response(),
        }
    };
    let (reply, confirmation) = tokio::sync::oneshot::channel();
    if stop_sender.send(reply).await.is_err() {
        return (StatusCode::CONFLICT, "Process already exited; refresh its state").into_response();
    }
    // A kill request is not an exit: only the owning monitor releases the slot.
    match confirmation.await {
        Ok(Ok(())) => (StatusCode::OK, "Stop requested").into_response(),
        Ok(Err(error)) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
        Err(_) => (StatusCode::CONFLICT, "Process exited before stop confirmation; refresh its state").into_response(),
    }
}

// Multipart upload handler for /deploy
use axum::extract::Multipart;

async fn deploy_model(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or_default().to_string();
        
        if name == "file" {
            let filename = field.file_name().unwrap_or("model.json").to_string();
            // Read all bytes into memory to parse metadata
            let data = match field.bytes().await {
                Ok(b) => b,
                Err(e) => return (StatusCode::BAD_REQUEST, format!("Failed to read upload: {}", e)).into_response(),
            };
            
            return match artifacts::upload_manual(&state.model_dir, &filename, &data) {
                Ok(filename) => (StatusCode::OK, format!("Deployed as {filename}")).into_response(),
                Err(error) => (StatusCode::BAD_REQUEST, format!("{error:#}")).into_response(),
            };
        }
    }
    
    (StatusCode::OK, "Deployed").into_response()
}

// Single Source of Truth: Retrieve Physics Configuration
async fn get_model_config(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match artifacts::read_legacy_config(&state.model_dir, &id) {
        Ok(content) => (StatusCode::OK, content).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, format!("{error:#}")).into_response(),
    }
}
// Get specific model file from controller's models directory
async fn get_model_file(
    State(state): State<AppState>,
    Path((controller_id, model_filename)): Path<(String, String)>,
) -> impl IntoResponse {
    match artifacts::load(&state.model_dir, &controller_id, &model_filename) {
        Ok(model) => (StatusCode::OK, model.bytes).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, format!("{error:#}")).into_response(),
    }
}

async fn install_control_version(
    State(state): State<AppState>,
    Json(payload): Json<artifacts::InstallRequest>,
) -> impl IntoResponse {
    // Installation is filesystem-only: no process launch, OPC writes, or mode changes.
    match runtime::install(&state, &payload) {
        Ok(receipt) => (StatusCode::OK, Json(receipt)).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, format!("{error:#}")).into_response(),
    }
}

#[cfg(test)]
mod process_tests {
    use super::*;

    #[cfg(unix)]
    fn monitored_child(mut command: Command) -> (AppState, tokio::task::JoinHandle<()>) {
        let child = command.kill_on_drop(true).spawn().unwrap();
        let pid = child.id().unwrap();
        let state = AppState {
            model_dir: PathBuf::new(),
            engine_bin: PathBuf::new(),
            pki_dir: PathBuf::new(),
            opc_defaults_path: PathBuf::new(),
            runtime_dir: PathBuf::new(),
            api_key: String::new(),
            opc_client_defaults: Arc::new(Mutex::new(OpcClientDefaults {
                opc_url: String::new(), auth_mode: String::new(), username: None,
                password: None, security_policy: String::new(), message_mode: String::new(),
            })),
            processes: Arc::new(Mutex::new(HashMap::from([("test".into(), ProcessStatus {
                id: "test".into(), active_model: None, model_sha256: None, control_version_id: None,
                state: ProcessState::Running(pid), last_error: None, started_at: Some(Utc::now()),
                stopped_at: None, last_log_path: None, stop_sender: None,
                authority: None,
            })]))),
        };
        let watcher = monitor_controller(child, state.processes.clone(), "test".into(), pid);
        (state, watcher)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn requested_stop_is_stopped_not_failed() {
        let mut command = Command::new("sleep");
        command.arg("30");
        let (state, watcher) = monitored_child(command);
        let response = stop_controller(State(state.clone()), Path("test".into())).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        tokio::time::timeout(std::time::Duration::from_secs(5), watcher).await.unwrap().unwrap();
        let processes = state.processes.lock().unwrap();
        let status = &processes["test"];
        assert_eq!(status.state, ProcessState::Stopped);
        assert!(status.last_error.is_none());
        assert!(status.stopped_at.is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unexpected_child_exit_is_failed() {
        for script in ["exit 7", "kill -TERM $$"] {
            let mut command = Command::new("sh");
            command.args(["-c", script]);
            let (state, watcher) = monitored_child(command);
            tokio::time::timeout(std::time::Duration::from_secs(5), watcher).await.unwrap().unwrap();
            let processes = state.processes.lock().unwrap();
            let status = &processes["test"];
            assert_eq!(status.state, ProcessState::Failed);
            assert!(status.last_error.is_some());
            assert!(status.stopped_at.is_some());
        }
    }

    #[test]
    fn start_slot_is_serialized_and_failed_start_releases_guard() {
        let processes = Arc::new(Mutex::new(HashMap::new()));
        {
            let _failed_start = reserve_start(&processes, "Kiln_A").unwrap();
            assert!(processes.try_lock().is_err());
        }
        let mut first = reserve_start(&processes, "Kiln_A").unwrap();
        let other = processes.clone();
        let second = std::thread::spawn(move || reserve_start(&other, "Kiln_A").is_err());
        first.insert("Kiln_A".into(), ProcessStatus {
            id: "Kiln_A".into(),
            active_model: Some("manual.json".into()),
            model_sha256: Some("a".repeat(64)),
            control_version_id: None,
            state: ProcessState::Running(1),
            last_error: None,
            started_at: None,
            stopped_at: None,
            last_log_path: None,
            stop_sender: None,
            authority: None,
        });
        drop(first);
        assert!(second.join().unwrap());
        assert!(reserve_start(&processes, "Other_Kiln").is_ok());
    }
}