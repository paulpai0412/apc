mod config;
mod auth;
mod opc_worker;
mod model_binding;
mod state;
mod web_routes;
mod historian;
mod infrastructure; // Added module
mod simulation_reports;
mod research;
mod research_postgres;
mod control_versions;
mod process_trends;
mod runtime;

use anyhow::{Context, Result};
use axum::{routing::{get, post, put}, Router};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc}; // Import mpsc
use tower_http::services::ServeDir;
use tower_sessions::{cookie::SameSite, Expiry, MemoryStore, SessionManagerLayer};
use time::Duration;
use web_routes::{get_model_config, list_models, get_trends_data};
use crate::config::AppSettings;
use crate::state::AppState;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    if let Some(command) = args.next() {
        anyhow::ensure!(command == "--hash-runtime-profiles", "Unknown argument");
        let input = args.next().context("Missing input profile file")?;
        let output = args.next().context("Missing output profile file")?;
        anyhow::ensure!(args.next().is_none(), "Unexpected profile hashing arguments");
        runtime::canonicalize_profiles(
            input.to_str().context("Profile input path must be UTF-8")?,
            output.to_str().context("Profile output path must be UTF-8")?,
        ).await.map_err(anyhow::Error::msg)?;
        return Ok(());
    }
    // 1. Load Settings
    println!("⚙️ Loading settings...");
    let config_path = std::env::var("AUTHRITY_HMI_CONFIG").unwrap_or_else(|_| "config/settings.toml".into());
    let settings = AppSettings::load(&config_path)?;
    let addr: SocketAddr = settings.web.bind_address.parse().context("Invalid web.bind_address")?;
    anyhow::ensure!(settings.hmi_auth.test_bootstrap_password.is_none()
        || (settings.web.local_test_mode && addr.ip().is_loopback()),
        "Test bootstrap credentials require explicit local test mode and a loopback listener");
    if settings.web.local_test_mode {
        anyhow::ensure!(addr.ip().is_loopback(), "Local test mode requires a loopback listener");
        let local_endpoint = |value: &str| reqwest::Url::parse(value).ok().and_then(|url| {
            url.host_str().map(|host| host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()))
        }).unwrap_or(false);
        anyhow::ensure!(!settings.runtime.connect_opcua || local_endpoint(&settings.opcua.endpoint_url),
            "Local test mode cannot connect to a non-loopback OPC endpoint");
        anyhow::ensure!(local_endpoint(&settings.services.supervisor_url) && local_endpoint(&settings.services.opc_hot_reload_url),
            "Local test mode requires loopback supervisor and provisioning endpoints");
        anyhow::ensure!(!settings.historian.enabled || settings.historian.host == "localhost"
            || settings.historian.host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()),
            "Local test mode cannot connect to a non-loopback historian");
    }
    let research = Arc::new(research::ResearchStore::new(&settings)?);
    research.reconcile_twins().await?;
    let profiles = runtime::RuntimeStore::load_profiles(settings.runtime.profiles_file.as_deref())
        .await.map_err(anyhow::Error::msg)?;
    anyhow::ensure!(!settings.web.local_test_mode
        || profiles.iter().all(|profile| profile.commissioning_kind == "synthetic_local"),
        "Local test mode refuses field commissioning profiles");
    let runtime = Arc::new(runtime::RuntimeStore::open(
        &settings.hmi_auth.database_url, profiles, !settings.research.enabled,
    ).await.map_err(anyhow::Error::msg)?);
    
    // 2. Create Channels
    // A. Broadcast Channel (One-to-Many): Sends OPC updates to all connected browsers
    let (tx, _rx) = broadcast::channel(5000);
    tokio::spawn(runtime.clone().observe(tx.subscribe()));

    // B. Command Channel (Many-to-One): Sends commands from Web to OPC Worker
    let (cmd_tx, cmd_rx) = mpsc::channel(100); 

    // 3. Spawn OPC Worker (Background Task)
    let opc_settings = settings.clone();
    let opc_tx = tx.clone();
    
    if settings.runtime.connect_opcua {
        tokio::spawn(async move {
            println!("Starting OPC UA Worker...");
            if let Err(e) = opc_worker::run_opc_worker(opc_settings, cmd_rx, opc_tx).await {
                eprintln!("OPC Worker Failed: {}", e);
            }
        });
    } else {
        println!("OPC connection disabled by explicit runtime configuration; local HTTP operation only.");
    }

    // 3b. Spawn Historian (Background Task)
    let historian_config = settings.historian.clone();
    let historian_rx = tx.subscribe();
    let historian_cmd_tx = cmd_tx.clone();
    
    tokio::spawn(async move {
        if let Err(e) = historian::run_historian(historian_config, historian_rx, historian_cmd_tx).await {
            eprintln!("❌ Historian Failed: {}", e);
        }
    });

    // 4. Shared State for Axum
    let auth_service = auth::AuthService::new(&settings.hmi_auth.database_url).await?;
    if let Some(password) = &settings.hmi_auth.test_bootstrap_password {
        auth_service.ensure_local_test_admin("admin", password, addr, settings.web.local_test_mode).await
            .map_err(|_| anyhow::anyhow!("Local test administrator initialization rejected"))?;
    } else {
        auth_service.ensure_bootstrap_admin("admin", "password").await
            .map_err(|_| anyhow::anyhow!("Bootstrap administrator initialization failed"))?;
    }

    if !settings.web.local_test_mode {
        infrastructure::InfrastructureConfig::seed_if_empty(
            &settings.hmi_auth.database_url, "config/hosts.json",
        ).await?;
    }

    // We store 'cmd_tx' here so the web routes can send commands
    let app_state = AppState {
        tx,
        cmd_tx, // <--- Added this to state
        settings: Arc::new(settings.clone()),
        historian_config: settings.historian.clone(),
        auth: auth::shared(auth_service),
        reports: Arc::new(simulation_reports::ReportStore::new(PathBuf::from(&settings.reports.directory))),
        research,
        runtime,
    };
    research::start_policy_scheduler(app_state.clone());
    runtime::publisher::spawn_scheduler(app_state.clone());

    let session_store = MemoryStore::default();
    let inactivity_minutes = settings.hmi_auth.session_inactivity_minutes.max(5) as i64;
    let session_layer = SessionManagerLayer::new(session_store)
        .with_name(format!("authrity_{}_{}", if settings.research.enabled { "research" } else { "runtime" }, addr.port()))
        .with_secure(!settings.web.local_test_mode)
        .with_same_site(SameSite::Strict)
        .with_expiry(Expiry::OnInactivity(Duration::minutes(inactivity_minutes)));

    // 5. Frontend Location
    let assets_dir = PathBuf::from(&settings.web.assets_directory);
    if !assets_dir.exists() {
        println!("⚠️  Frontend folder '{:?}' not found. Serving 404s.", assets_dir);
    }

    // 6. Define Routes
    let app = Router::new()
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/me", get(auth::me))
        .route("/api/auth/change-password", post(auth::change_password))
        .route("/api/admin/audit", get(web_routes::list_audit_admin))
        .route("/api/admin/users", get(web_routes::list_users_admin).post(web_routes::create_user_admin))
        .route("/api/admin/users/:id/role", post(web_routes::update_user_role_admin))
        .route("/api/admin/users/:id/disable", post(web_routes::set_user_disabled_admin))
        .route("/api/admin/users/:id/password", post(web_routes::reset_user_password_admin))
        .route("/ws", get(web_routes::ws_handler))
        .route("/api/simulation-reports", get(simulation_reports::list))
        .route("/api/simulation-reports/:id", get(simulation_reports::manifest))
        .route("/api/simulation-reports/:id/series", get(simulation_reports::series))
        .route("/api/simulation-reports/:id/artifacts/:name", get(simulation_reports::artifact))
        .merge(research::routes())
        .merge(control_versions::routes())
        .merge(runtime::routes())
        .route("/api/models", get(list_models))
        .route("/api/model", get(get_model_config))
        .route("/api/controller/model", get(web_routes::get_controller_model)) // <--- Get Active Model
        .route("/api/controller/:controller_id/models/:model_filename", get(web_routes::get_controller_model_file)) // <--- Get Specific Model File
        .route("/api/trends", get(get_trends_data))
        .route("/api/stepresponse/data", get(web_routes::get_step_response_data))
        .route("/api/stepresponse/tags", get(web_routes::get_available_tags_api))
        .route("/api/stepresponse/calculate", post(web_routes::calculate_step_response))
        .route("/api/infrastructure", get(web_routes::get_infrastructure).post(web_routes::save_infrastructure)) // <--- Infrastructure Registry
        .route("/api/infrastructure/controller-host-clients", post(web_routes::save_controller_host_clients))
        .route("/api/infrastructure/opc-reconnect", post(web_routes::reconnect_opc_worker))
        .route("/api/prox/opc/security/config", get(web_routes::get_opc_security_config_proxy).put(web_routes::save_opc_security_config_proxy))
        .route("/api/prox/opc/security/tokens", get(web_routes::list_opc_user_tokens_proxy).post(web_routes::upsert_opc_user_token_proxy))
        .route("/api/prox/opc/security/tokens/:id", put(web_routes::upsert_opc_user_token_by_id_proxy).delete(web_routes::delete_opc_user_token_proxy))
        .route("/api/prox/opc/restart", post(web_routes::restart_opc_server_proxy))
        .route("/api/physics/:id", get(web_routes::get_remote_physics)) // <--- Fetch JSON from Supervisor
        // NEW: Controller Management Proxy
        .route("/api/prox/controllers", get(web_routes::list_controllers_proxy))
        .route("/api/prox/controllers/:id/logs/tail", get(web_routes::get_controller_logs_proxy))
        .nest_service("/", ServeDir::new(assets_dir))
        .with_state(app_state)
        .layer(session_layer);

    // 7. Start HTTP Server
    let listener = tokio::net::TcpListener::bind(addr).await?;
    
    println!("HMI Server running at http://{}", addr);
    axum::serve(listener, app).await?;

    Ok(())
}