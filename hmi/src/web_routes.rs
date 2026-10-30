use axum::{
    extract::{ws::{Message, WebSocket, WebSocketUpgrade}, State, Query, Path as AxumPath},
    response::IntoResponse,
    Json,
    http::StatusCode,
};
use crate::state::AppState;
use crate::opc_worker::{normalize_auth_mode, NodesFile, OpcCommand};
use futures::{sink::SinkExt, stream::StreamExt}; 
use serde::Deserialize;
use reqwest::Client;
use std::collections::HashSet;
use crate::infrastructure::InfrastructureConfig; // Added Import
use crate::auth::{CsrfVerified, CurrentUser, Permission, Role};
use crate::model_binding::RuntimeBindings;

#[derive(Debug, Deserialize)]
pub struct CreateUserRequest {
    pub username: String,
    pub password: String,
    pub role: Role,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRoleRequest {
    pub role: Role,
}

#[derive(Debug, Deserialize)]
pub struct ResetPasswordRequest {
    pub new_password: String,
}

#[derive(Debug, Deserialize)]
pub struct SetDisabledRequest {
    pub disabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    pub limit: Option<i64>,
    pub action: Option<String>,
    pub result: Option<String>,
    pub actor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ControllerModelQuery {
    pub controller_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SaveControllerHostClientsRequest {
    pub supervisors: Vec<crate::infrastructure::ServiceConfig>,
    pub controller_host_clients: Vec<crate::infrastructure::ControllerHostClientSettings>,
}

#[derive(Debug, Deserialize)]
pub struct OpcServerSelectorQuery {
    pub opc_server_id: Option<String>,
}

fn normalize_service_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_ascii_lowercase()
}

fn validate_unique_opc_servers(infra: &InfrastructureConfig) -> Result<(), String> {
    let mut seen_admin_urls: HashSet<String> = HashSet::new();
    let mut seen_opc_endpoints: HashSet<String> = HashSet::new();

    for opc in &infra.opc_servers {
        let admin_url = normalize_service_url(&opc.url);
        if admin_url.is_empty() {
            return Err(format!("OPC server '{}' has an empty admin URL", opc.name));
        }
        if !seen_admin_urls.insert(admin_url.clone()) {
            return Err(format!("Duplicate OPC server admin URL is not allowed: {}", opc.url));
        }

        if let Some(endpoint) = opc.opc_endpoint.as_ref() {
            let endpoint_norm = normalize_service_url(endpoint);
            if !endpoint_norm.is_empty() && !seen_opc_endpoints.insert(endpoint_norm) {
                return Err(format!("Duplicate OPC endpoint is not allowed: {}", endpoint));
            }
        }
    }

    Ok(())
}

pub async fn list_audit_admin(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<AuditQuery>,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::AuditRead) {
        return e.into_response();
    }

    let limit = query.limit.unwrap_or(200);
    match state
        .auth
        .list_audit_events(
            limit,
            query.action.as_deref(),
            query.result.as_deref(),
            query.actor.as_deref(),
        )
        .await
    {
        Ok(events) => Json(events).into_response(),
        Err(e) => e.into_response(),
    }
}

pub async fn list_users_admin(
    State(state): State<AppState>,
    user: CurrentUser,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::UserManage) {
        return e.into_response();
    }

    match state.auth.list_users().await {
        Ok(users) => Json(users).into_response(),
        Err(e) => e.into_response(),
    }
}

pub async fn create_user_admin(
    State(state): State<AppState>,
    _: CsrfVerified,
    user: CurrentUser,
    Json(payload): Json<CreateUserRequest>,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::UserManage) {
        return e.into_response();
    }

    match state
        .auth
        .create_user(payload.username.trim(), &payload.password, payload.role)
        .await
    {
        Ok(_) => {
            state
                .auth
                .audit(Some(&user), "user.create", Some(payload.username.trim()), "success", None)
                .await;
            StatusCode::CREATED.into_response()
        }
        Err(e) => e.into_response(),
    }
}

pub async fn update_user_role_admin(
    State(state): State<AppState>,
    _: CsrfVerified,
    user: CurrentUser,
    AxumPath(id): AxumPath<i64>,
    Json(payload): Json<UpdateRoleRequest>,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::UserManage) {
        return e.into_response();
    }

    match state.auth.update_user_role(id, payload.role).await {
        Ok(_) => {
            state
                .auth
                .audit(Some(&user), "user.update_role", Some(&id.to_string()), "success", None)
                .await;
            StatusCode::OK.into_response()
        }
        Err(e) => e.into_response(),
    }
}

pub async fn set_user_disabled_admin(
    State(state): State<AppState>,
    _: CsrfVerified,
    user: CurrentUser,
    AxumPath(id): AxumPath<i64>,
    Json(payload): Json<SetDisabledRequest>,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::UserManage) {
        return e.into_response();
    }

    if id == user.id && payload.disabled {
        return (StatusCode::BAD_REQUEST, "Cannot disable current user").into_response();
    }

    match state.auth.set_user_disabled(id, payload.disabled).await {
        Ok(_) => {
            state
                .auth
                .audit(Some(&user), "user.set_disabled", Some(&id.to_string()), "success", Some(if payload.disabled { "disabled" } else { "enabled" }))
                .await;
            StatusCode::OK.into_response()
        }
        Err(e) => e.into_response(),
    }
}

pub async fn reset_user_password_admin(
    State(state): State<AppState>,
    _: CsrfVerified,
    user: CurrentUser,
    AxumPath(id): AxumPath<i64>,
    Json(payload): Json<ResetPasswordRequest>,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::UserManage) {
        return e.into_response();
    }

    match state.auth.reset_user_password(id, &payload.new_password).await {
        Ok(_) => {
            state
                .auth
                .audit(Some(&user), "user.reset_password", Some(&id.to_string()), "success", None)
                .await;
            StatusCode::OK.into_response()
        }
        Err(e) => e.into_response(),
    }
}

// --- INFRASTRUCTURE HANDLERS ---
pub async fn get_infrastructure(
    State(state): State<AppState>,
    user: CurrentUser,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::InfraWrite) {
        return e.into_response();
    }

    let config = match InfrastructureConfig::load_from_db(&state.settings.hmi_auth.database_url).await {
        Ok(config) => config,
        Err(_) => {
            let fallback = InfrastructureConfig::load("config/hosts.json").await.unwrap_or_default();
            if let Err(e) = fallback.save_to_db(&state.settings.hmi_auth.database_url).await {
                return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to initialize infrastructure settings: {}", e)).into_response();
            }
            fallback
        }
    };

    state
        .auth
        .audit(Some(&user), "infra.read", Some("sqlite:infra_settings"), "success", None)
        .await;

    Json(config.redacted_for_api()).into_response()
}

pub async fn save_infrastructure(
    State(state): State<AppState>,
    _: CsrfVerified,
    user: CurrentUser,
    Json(payload): Json<InfrastructureConfig>,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::InfraWrite) {
        return e.into_response();
    }
    let _maintenance=match crate::runtime::safety::maintenance(&state,&user).await {Ok(guard)=>guard,Err(error)=>return error.into_response()};

    if let Err(detail) = validate_unique_opc_servers(&payload) {
        return (StatusCode::BAD_REQUEST, detail).into_response();
    }

    let existing = InfrastructureConfig::load_from_db(&state.settings.hmi_auth.database_url)
        .await
        .ok();
    let merged_payload = payload.with_secret_refs(existing.as_ref());

    match merged_payload.save_to_db(&state.settings.hmi_auth.database_url).await {
        Ok(_) => {
            if let Err(sync_err) = sync_controller_host_opc_defaults(
                &merged_payload,
                &state.settings.services.supervisor_api_key,
            )
            .await
            {
                state
                    .auth
                    .audit(
                        Some(&user),
                        "infra.write",
                        Some("sqlite:infra_settings"),
                        "partial",
                        Some(&format!("saved but sync failed: {}", sync_err)),
                    )
                    .await;
                return (
                    StatusCode::BAD_GATEWAY,
                    format!("Infrastructure saved, but Controller Host sync failed: {}", sync_err),
                )
                    .into_response();
            }

            state
                .auth
                .audit(Some(&user), "infra.write", Some("sqlite:infra_settings"), "success", None)
                .await;
            StatusCode::OK.into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to save infrastructure to DB: {}", e)).into_response(),
    }
}

pub async fn save_controller_host_clients(
    State(state): State<AppState>,
    _: CsrfVerified,
    user: CurrentUser,
    Json(payload): Json<SaveControllerHostClientsRequest>,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::InfraWrite) {
        return e.into_response();
    }
    let _maintenance=match crate::runtime::safety::maintenance(&state,&user).await {Ok(guard)=>guard,Err(error)=>return error.into_response()};

    let mut config = match InfrastructureConfig::load_from_db(&state.settings.hmi_auth.database_url).await {
        Ok(current) => current,
        Err(_) => InfrastructureConfig::load("config/hosts.json").await.unwrap_or_default(),
    };

    config.supervisors = payload.supervisors;
    config.controller_host_clients = payload.controller_host_clients;

    let existing = InfrastructureConfig::load_from_db(&state.settings.hmi_auth.database_url)
        .await
        .ok();
    let merged_payload = config.with_secret_refs(existing.as_ref());

    match merged_payload.save_to_db(&state.settings.hmi_auth.database_url).await {
        Ok(_) => {
            if let Err(sync_err) = sync_controller_host_opc_defaults(
                &merged_payload,
                &state.settings.services.supervisor_api_key,
            )
            .await
            {
                state
                    .auth
                    .audit(
                        Some(&user),
                        "infra.controller_host_clients.write",
                        Some("sqlite:infra_settings"),
                        "partial",
                        Some(&format!("saved but sync failed: {}", sync_err)),
                    )
                    .await;
                return (
                    StatusCode::BAD_GATEWAY,
                    format!("Controller Host clients saved, but sync failed: {}", sync_err),
                )
                    .into_response();
            }

            state
                .auth
                .audit(
                    Some(&user),
                    "infra.controller_host_clients.write",
                    Some("sqlite:infra_settings"),
                    "success",
                    None,
                )
                .await;

            StatusCode::OK.into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to save controller host clients to DB: {}", e),
        )
            .into_response(),
    }
}

async fn sync_controller_host_opc_defaults(
    infra: &InfrastructureConfig,
    api_key: &str,
) -> std::result::Result<(), String> {
    let client = Client::new();
    let mut failures: Vec<String> = Vec::new();

    let opc_url = infra
        .opc_servers
        .first()
        .and_then(|opc| opc.opc_endpoint.clone())
        .filter(|value| !value.trim().is_empty());

    for supervisor in &infra.supervisors {
        let Some(host_client) = infra
            .controller_host_clients
            .iter()
            .find(|candidate| candidate.supervisor_id == supervisor.id)
        else {
            continue;
        };

        let (username, password) = infra.resolve_controller_host_username_password(&supervisor.id);
        let url = format!("{}/api/opc-client/defaults", supervisor.url.trim_end_matches('/'));

        let body = serde_json::json!({
            "opc_url": opc_url.clone(),
            "security_policy": host_client.security_policy,
            "message_mode": host_client.security_mode,
            "auth_mode": normalize_auth_mode(&host_client.auth_mode),
            "username": username,
            "password": password,
        });

        match client
            .post(&url)
            .header("x-api-key", api_key)
            .json(&body)
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {}
            Ok(response) => {
                let status = response.status();
                let detail = response.text().await.unwrap_or_else(|_| "no response body".to_string());
                failures.push(format!("{} -> {}: {}", supervisor.name, status, detail));
            }
            Err(err) => failures.push(format!("{} -> request error: {}", supervisor.name, err)),
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

pub async fn reconnect_opc_worker(
    State(state): State<AppState>,
    _: CsrfVerified,
    user: CurrentUser,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::InfraWrite) {
        return e.into_response();
    }
    let _maintenance=match crate::runtime::safety::maintenance(&state,&user).await {Ok(guard)=>guard,Err(error)=>return error.into_response()};

    let baseline_connection=state.runtime.worker_connection().await;
    let queued_at=chrono::Utc::now();
    match state.cmd_tx.try_send(OpcCommand::Reconnect) {
        Ok(())=>{
            state.auth.audit(Some(&user),"infra.opc_reconnect",Some("opc_worker"),"queued",
                Some("Reconnect requested; queue acceptance is not connection confirmation")).await;
            (StatusCode::ACCEPTED,Json(serde_json::json!({"state":"queued","connected":null,"confirmed":false,
                "queued_at":queued_at,"baseline_connection":baseline_connection,
                "detail":"Reconnect queued. Observe subsequent worker connection events; this response does not confirm a connection."}))).into_response()
        },
        Err(error)=>(StatusCode::SERVICE_UNAVAILABLE,Json(serde_json::json!({"state":"not_queued","confirmed":false,
            "detail":format!("Reconnect was not queued: {error}")}))).into_response(),
    }
}

fn select_opc_admin_url(infra: &InfrastructureConfig, opc_server_id: Option<&str>) -> Option<String> {
    let selected = if let Some(id) = opc_server_id.map(str::trim).filter(|id| !id.is_empty()) {
        infra.opc_servers.iter().find(|opc| opc.id == id)
    } else {
        infra.opc_servers.first()
    };

    selected
        .map(|opc| opc.url.trim().trim_end_matches('/').to_string())
        .filter(|url| !url.is_empty())
}

pub async fn get_opc_security_config_proxy(
    State(state): State<AppState>,
    Query(query): Query<OpcServerSelectorQuery>,
    user: CurrentUser,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::InfraWrite) {
        return e.into_response();
    }

    let infra = match InfrastructureConfig::load_from_db(&state.settings.hmi_auth.database_url).await {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to load infrastructure config: {}", e))
                .into_response();
        }
    };

    let Some(opc_admin_url) = select_opc_admin_url(&infra, query.opc_server_id.as_deref()) else {
        return (StatusCode::NOT_FOUND, "No OPC server configured in infrastructure settings").into_response();
    };

    let client = Client::new();
    let url = format!("{}/api/security/config", opc_admin_url);
    match client.get(&url).send().await {
        Ok(resp) => {
            let status = resp.status();
            match resp.text().await {
                Ok(body) => (status, body).into_response(),
                Err(e) => (StatusCode::BAD_GATEWAY, format!("Failed to read OPC response: {}", e)).into_response(),
            }
        }
        Err(e) => (StatusCode::BAD_GATEWAY, format!("OPC server unreachable: {}", e)).into_response(),
    }
}

pub async fn save_opc_security_config_proxy(
    State(state): State<AppState>,
    Query(query): Query<OpcServerSelectorQuery>,
    _: CsrfVerified,
    user: CurrentUser,
    Json(payload): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::InfraWrite) {
        return e.into_response();
    }
    let _maintenance=match crate::runtime::safety::maintenance(&state,&user).await {Ok(guard)=>guard,Err(error)=>return error.into_response()};

    let infra = match InfrastructureConfig::load_from_db(&state.settings.hmi_auth.database_url).await {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to load infrastructure config: {}", e))
                .into_response();
        }
    };

    let Some(opc_admin_url) = select_opc_admin_url(&infra, query.opc_server_id.as_deref()) else {
        return (StatusCode::NOT_FOUND, "No OPC server configured in infrastructure settings").into_response();
    };

    let client = Client::new();
    let url = format!("{}/api/security/config", opc_admin_url);
    match client.put(&url).json(&payload).send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                state
                    .auth
                    .audit(Some(&user), "infra.opc_security.write", Some(&opc_admin_url), "success", None)
                    .await;
            }
            match resp.text().await {
                Ok(body) => (status, body).into_response(),
                Err(e) => (StatusCode::BAD_GATEWAY, format!("Failed to read OPC response: {}", e)).into_response(),
            }
        }
        Err(e) => (StatusCode::BAD_GATEWAY, format!("OPC server unreachable: {}", e)).into_response(),
    }
}

pub async fn list_opc_user_tokens_proxy(
    State(state): State<AppState>,
    Query(query): Query<OpcServerSelectorQuery>,
    user: CurrentUser,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::InfraWrite) {
        return e.into_response();
    }

    let infra = match InfrastructureConfig::load_from_db(&state.settings.hmi_auth.database_url).await {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to load infrastructure config: {}", e))
                .into_response();
        }
    };

    let Some(opc_admin_url) = select_opc_admin_url(&infra, query.opc_server_id.as_deref()) else {
        return (StatusCode::NOT_FOUND, "No OPC server configured in infrastructure settings").into_response();
    };

    let client = Client::new();
    let url = format!("{}/api/security/tokens", opc_admin_url);
    match client.get(&url).send().await {
        Ok(resp) => {
            let status = resp.status();
            match resp.text().await {
                Ok(body) => (status, body).into_response(),
                Err(e) => (StatusCode::BAD_GATEWAY, format!("Failed to read OPC response: {}", e)).into_response(),
            }
        }
        Err(e) => (StatusCode::BAD_GATEWAY, format!("OPC server unreachable: {}", e)).into_response(),
    }
}

pub async fn upsert_opc_user_token_proxy(
    State(state): State<AppState>,
    Query(query): Query<OpcServerSelectorQuery>,
    _: CsrfVerified,
    user: CurrentUser,
    Json(payload): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::InfraWrite) {
        return e.into_response();
    }
    let _maintenance=match crate::runtime::safety::maintenance(&state,&user).await {Ok(guard)=>guard,Err(error)=>return error.into_response()};

    let infra = match InfrastructureConfig::load_from_db(&state.settings.hmi_auth.database_url).await {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to load infrastructure config: {}", e))
                .into_response();
        }
    };

    let Some(opc_admin_url) = select_opc_admin_url(&infra, query.opc_server_id.as_deref()) else {
        return (StatusCode::NOT_FOUND, "No OPC server configured in infrastructure settings").into_response();
    };

    let client = Client::new();
    let url = format!("{}/api/security/tokens", opc_admin_url);
    match client.post(&url).json(&payload).send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                let target = payload.get("id").and_then(|v| v.as_str()).unwrap_or("<generated>");
                state
                    .auth
                    .audit(Some(&user), "infra.opc_token.upsert", Some(target), "success", None)
                    .await;
            }
            match resp.text().await {
                Ok(body) => (status, body).into_response(),
                Err(e) => (StatusCode::BAD_GATEWAY, format!("Failed to read OPC response: {}", e)).into_response(),
            }
        }
        Err(e) => (StatusCode::BAD_GATEWAY, format!("OPC server unreachable: {}", e)).into_response(),
    }
}

pub async fn upsert_opc_user_token_by_id_proxy(
    AxumPath(id): AxumPath<String>,
    State(state): State<AppState>,
    Query(query): Query<OpcServerSelectorQuery>,
    _: CsrfVerified,
    user: CurrentUser,
    Json(payload): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::InfraWrite) {
        return e.into_response();
    }
    let _maintenance=match crate::runtime::safety::maintenance(&state,&user).await {Ok(guard)=>guard,Err(error)=>return error.into_response()};

    let infra = match InfrastructureConfig::load_from_db(&state.settings.hmi_auth.database_url).await {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to load infrastructure config: {}", e))
                .into_response();
        }
    };

    let Some(opc_admin_url) = select_opc_admin_url(&infra, query.opc_server_id.as_deref()) else {
        return (StatusCode::NOT_FOUND, "No OPC server configured in infrastructure settings").into_response();
    };

    let client = Client::new();
    let url = format!("{}/api/security/tokens/{}", opc_admin_url, id);
    match client.put(&url).json(&payload).send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                state
                    .auth
                    .audit(Some(&user), "infra.opc_token.upsert", Some(&id), "success", None)
                    .await;
            }
            match resp.text().await {
                Ok(body) => (status, body).into_response(),
                Err(e) => (StatusCode::BAD_GATEWAY, format!("Failed to read OPC response: {}", e)).into_response(),
            }
        }
        Err(e) => (StatusCode::BAD_GATEWAY, format!("OPC server unreachable: {}", e)).into_response(),
    }
}

pub async fn delete_opc_user_token_proxy(
    AxumPath(id): AxumPath<String>,
    State(state): State<AppState>,
    Query(query): Query<OpcServerSelectorQuery>,
    _: CsrfVerified,
    user: CurrentUser,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::InfraWrite) {
        return e.into_response();
    }
    let _maintenance=match crate::runtime::safety::maintenance(&state,&user).await {Ok(guard)=>guard,Err(error)=>return error.into_response()};

    let infra = match InfrastructureConfig::load_from_db(&state.settings.hmi_auth.database_url).await {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to load infrastructure config: {}", e))
                .into_response();
        }
    };

    let Some(opc_admin_url) = select_opc_admin_url(&infra, query.opc_server_id.as_deref()) else {
        return (StatusCode::NOT_FOUND, "No OPC server configured in infrastructure settings").into_response();
    };

    let client = Client::new();
    let url = format!("{}/api/security/tokens/{}", opc_admin_url, id);
    match client.delete(&url).send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                state
                    .auth
                    .audit(Some(&user), "infra.opc_token.delete", Some(&id), "success", None)
                    .await;
            }
            match resp.text().await {
                Ok(body) => (status, body).into_response(),
                Err(e) => (StatusCode::BAD_GATEWAY, format!("Failed to read OPC response: {}", e)).into_response(),
            }
        }
        Err(e) => (StatusCode::BAD_GATEWAY, format!("OPC server unreachable: {}", e)).into_response(),
    }
}

pub async fn restart_opc_server_proxy(
    State(state): State<AppState>,
    Query(query): Query<OpcServerSelectorQuery>,
    _: CsrfVerified,
    user: CurrentUser,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::InfraWrite) {
        return e.into_response();
    }
    let _maintenance=match crate::runtime::safety::maintenance(&state,&user).await {Ok(guard)=>guard,Err(error)=>return error.into_response()};

    let infra = match InfrastructureConfig::load_from_db(&state.settings.hmi_auth.database_url).await {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to load infrastructure config: {}", e))
                .into_response();
        }
    };

    let Some(opc_admin_url) = select_opc_admin_url(&infra, query.opc_server_id.as_deref()) else {
        return (StatusCode::NOT_FOUND, "No OPC server configured in infrastructure settings").into_response();
    };

    let client = Client::new();
    let url = format!("{}/api/admin/restart", opc_admin_url);
    match client.post(&url).send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                state
                    .auth
                    .audit(Some(&user), "infra.opc_server.restart", Some(&opc_admin_url), "success", None)
                    .await;
            }
            match resp.text().await {
                Ok(body) => (status, body).into_response(),
                Err(e) => (StatusCode::BAD_GATEWAY, format!("Failed to read OPC response: {}", e)).into_response(),
            }
        }
        Err(e) => (StatusCode::BAD_GATEWAY, format!("OPC server unreachable: {}", e)).into_response(),
    }
}


// --- WEBSOCKET HANDLER ---
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    user: CurrentUser,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::ReadView) {
        return e.into_response();
    }
    if let Err(error)=crate::runtime::authorize_legacy_read(&state,&user){return error.into_response();}
    let Some(identity)=state.auth.get_user_auth_state(user.id).await.ok().flatten()else{
        return (StatusCode::UNAUTHORIZED,"Session identity unavailable").into_response();
    };
    let auth_version=identity.auth_version;
    ws.on_upgrade(move|socket|handle_socket(socket,state,user,auth_version))
}

async fn handle_socket(socket: WebSocket, state: AppState,user:CurrentUser,auth_version:i64) {
    let (mut sender, mut receiver) = socket.split();
    let mut rx = state.tx.subscribe();

    let cmd_tx = state.cmd_tx.clone();
    
    // 1. Receive Commands (Browser -> Rust)
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            if let Message::Text(text) = msg {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    if json["type"] == "REFRESH" {
                        println!("🔄 Web Command: Client requested data refresh");
                        let _ = cmd_tx.send(OpcCommand::ReadAll).await;
                    }
                }
            }
        }
    });

    // 2. Send Updates (Rust -> Browser)
    let mut send_task = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(msg) => {
                    if let Ok(json_text) = serde_json::to_string(&msg) {
                        if sender.send(Message::Text(json_text)).await.is_err() { break; }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });

    tokio::select! {
        _=&mut recv_task=>{}, _=&mut send_task=>{},
        _=async {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let valid=state.auth.get_user_auth_state(user.id).await.ok().flatten()
                    .is_some_and(|current|!current.disabled&&!current.force_password_change&&current.role==user.role&&current.auth_version==auth_version);
                if !valid||crate::runtime::authorize_legacy_read(&state,&user).is_err(){break;}
            }
        }=>{}
    }
    recv_task.abort();
    send_task.abort();
}

struct ConfiguredModelSource {
    nodes_url: reqwest::Url,
    endpoint: String,
}

async fn configured_model_source(state: &AppState) -> Result<ConfiguredModelSource, String> {
    let infra = InfrastructureConfig::load_from_db(&state.settings.hmi_auth.database_url).await
        .map_err(|error| format!("Model source configuration is unavailable: {error}"))?;
    let opc = infra.opc_servers.first()
        .ok_or("No OPC server configured in infrastructure settings")?;
    let nodes_url = reqwest::Url::parse(&format!("{}/api/nodes", opc.url.trim_end_matches('/')))
        .map_err(|_| "Configured OPC management URL is invalid")?;
    // Match the worker's existing primary-source and settings fallback resolution.
    let endpoint = opc.opc_endpoint.as_ref().filter(|endpoint| !endpoint.is_empty())
        .unwrap_or(&state.settings.opcua.endpoint_url).clone();
    if endpoint.is_empty() {
        return Err("Configured OPC endpoint is missing; runtime writes are disabled".into());
    }
    Ok(ConfiguredModelSource { nodes_url, endpoint })
}

fn model_source_client() -> Result<Client, String> {
    Client::builder().timeout(std::time::Duration::from_millis(500)).build()
        .map_err(|error| format!("Cannot initialize OPC model lookup: {error}"))
}

async fn fetch_model_names(client: &Client, source: &ConfiguredModelSource) -> Result<Vec<String>, String> {
    client.get(source.nodes_url.clone()).send().await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| format!("Configured OPC model list is unavailable: {error}"))?
        .json::<Vec<String>>().await
        .map_err(|error| format!("Configured OPC model list is invalid: {error}"))
}

fn model_nodes_url(source: &ConfiguredModelSource, filename: &str) -> Result<reqwest::Url, String> {
    if filename.trim().is_empty() || filename == "." || filename == ".."
        || filename.contains(['/', '\\']) || filename.chars().any(char::is_control) {
        return Err("Invalid model filename; select a model from the configured OPC source".into());
    }
    let mut url = source.nodes_url.clone();
    url.path_segments_mut().map_err(|_| "Configured OPC management URL cannot contain model paths")?
        .push(filename);
    Ok(url)
}

async fn fetch_model_nodes(client: &Client, source: &ConfiguredModelSource, filename: &str) -> Result<NodesFile, String> {
    let url = model_nodes_url(source, filename)?;
    let content = client.get(url).send().await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| format!("Model '{filename}' is unavailable from the configured OPC source: {error}"))?
        .text().await.map_err(|error| format!("Cannot read model '{filename}': {error}"))?;
    serde_yaml::from_str::<NodesFile>(&content)
        .map_err(|error| format!("Invalid node map or model binding in '{filename}': {error}"))
}

// --- LIST ALL YAML MODELS ---
pub async fn list_models(
    State(state): State<AppState>,
    user: CurrentUser,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::ReadView) {
        return e.into_response();
    }

    let result = async {
        let source = configured_model_source(&state).await?;
        fetch_model_names(&model_source_client()?, &source).await
    }.await;
    match result {
        Ok(models) => Json(models).into_response(),
        Err(error) => (StatusCode::BAD_GATEWAY, error).into_response(),
    }
}

// --- DYNAMIC CONFIG LOADER ---
#[derive(Deserialize)]
pub struct ModelParams {
    file: Option<String>,
}

pub async fn get_model_config(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(params): Query<ModelParams>,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::ReadView) {
        return e.into_response();
    }

    let filename = match params.file {
        Some(value) if !value.trim().is_empty() => value,
        _ => return (StatusCode::BAD_REQUEST, "Missing required query parameter: file").into_response(),
    };
    
    let result = async {
        let source = configured_model_source(&state).await?;
        let nodes = fetch_model_nodes(&model_source_client()?, &source, &filename).await?;
        RuntimeBindings::from_models(std::slice::from_ref(&nodes))?;
        Ok::<_, String>(nodes)
    }.await;
    match result {
        Ok(nodes) => Json(nodes).into_response(),
        Err(error) => (StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error":error}))).into_response(),
    }
}

// --- FETCH PHYSICS FROM SUPERVISOR ---
pub async fn get_remote_physics(
    State(state): State<AppState>,
    user: CurrentUser,
    AxumPath(id): AxumPath<String>,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::ReadView) {
        return e.into_response();
    }

    // 1. Get primary supervisor from registry
    if let Ok(infra) = InfrastructureConfig::load_from_db(&state.settings.hmi_auth.database_url).await {
        if let Some(sup) = infra.supervisors.first() {
            let client = Client::new();
            let url = format!("{}/api/controllers/{}/config", sup.url, id);
            println!("🔍 Fetching Physics JSON from Supervisor: {}", url);
            
            match client.get(&url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(content) = resp.text().await {
                        // Return raw JSON (browser will parse it)
                        return (
                            [(axum::http::header::CONTENT_TYPE, "application/json")],
                            content
                        ).into_response();
                    }
                },
                Err(e) => println!("⚠️ Failed to fetch physics from remote Supervisor: {}", e),
                Ok(r) => println!("⚠️ Remote fetch failed: {}", r.status()),
            }
        }
    }
    
    (StatusCode::NOT_FOUND, "Physics config not found on supervisor").into_response()
}

// --- GET ACTIVE CONTROLLER MODEL ---
pub async fn get_controller_model(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<ControllerModelQuery>,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::ReadView) {
        return e.into_response();
    }

    // Get the active controller's model from supervisor
    let target_sup = state.settings.services.supervisor_url.clone();
    let api_key = state.settings.services.supervisor_api_key.clone();

    let client = Client::new();
    
    // First, list controllers to find a running target
    let list_url = format!("{}/api/controllers", target_sup);
    
    match client.get(&list_url).header("x-api-key", &api_key).send().await {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(controllers) = resp.json::<Vec<serde_json::Value>>().await {
                let requested_id = query.controller_id.as_deref().map(str::trim).filter(|s| !s.is_empty());

                let is_running = |c: &&serde_json::Value| {
                    c.get("state").map(|s| s.is_object()).unwrap_or(false)
                };

                let target = if let Some(controller_id) = requested_id {
                    controllers.iter().find(|c| c.get("id").and_then(|i| i.as_str()) == Some(controller_id))
                } else {
                    controllers.iter().find(is_running)
                };

                let Some(controller) = target else {
                    if let Some(controller_id) = requested_id {
                        return (StatusCode::NOT_FOUND, format!("Controller '{}' not found", controller_id)).into_response();
                    }
                    return (StatusCode::NOT_FOUND, "No active controller found").into_response();
                };

                if !is_running(&controller) {
                    let controller_id = controller.get("id").and_then(|i| i.as_str()).unwrap_or("unknown");
                    return (StatusCode::NOT_FOUND, format!("Controller '{}' is not running", controller_id)).into_response();
                }

                if let Some(id) = controller.get("id").and_then(|i| i.as_str()) {
                    if let Some(active_model) = controller.get("active_model").and_then(|m| m.as_str()) {
                        // Use the models endpoint which handles subdirectories correctly
                        let model_url = format!("{}/api/controllers/{}/models/{}", target_sup, id, active_model);
                        println!("🔍 Fetching active model from: {}", model_url);

                        match client.get(&model_url).header("x-api-key", &api_key).send().await {
                            Ok(model_resp) if model_resp.status().is_success() => {
                                if let Ok(model_json) = model_resp.text().await {
                                    return (
                                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                                        model_json
                                    ).into_response();
                                }
                            }
                            Err(e) => {
                                println!("⚠️ Failed to fetch model: {}", e);
                                return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to fetch model: {}", e)).into_response();
                            }
                            Ok(r) => {
                                println!("⚠️ Model fetch failed: {}", r.status());
                                return (StatusCode::BAD_GATEWAY, format!("Failed to fetch model from supervisor: {}", r.status())).into_response();
                            }
                        }
                    }
                }
                
                return (StatusCode::NOT_FOUND, "No active model found for controller").into_response();
            }
        }
        Err(e) => {
            println!("⚠️ Failed to list controllers: {}", e);
            return (StatusCode::BAD_GATEWAY, format!("Supervisor unreachable: {}", e)).into_response();
        }
        Ok(r) => {
            println!("⚠️ Controller list failed: {}", r.status());
            return (StatusCode::BAD_GATEWAY, "Failed to list controllers").into_response();
        }
    }
    
    (StatusCode::INTERNAL_SERVER_ERROR, "Unexpected error").into_response()
}

// --- GET SPECIFIC MODEL FILE FROM CONTROLLER ---
pub async fn get_controller_model_file(
    State(state): State<AppState>,
    user: CurrentUser,
    AxumPath((controller_id, model_filename)): AxumPath<(String, String)>,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::ReadView) {
        return e.into_response();
    }

    // Fetch a specific model file from a controller's models directory
    let target_sup = state.settings.services.supervisor_url.clone();
    let api_key = state.settings.services.supervisor_api_key.clone();
    let client = Client::new();
    
    // The supervisor's controller_host stores models in controller_host/models/{controller_id}/{model_filename}
    // We need to fetch the file content
    let model_url = format!("{}/api/controllers/{}/models/{}", target_sup, controller_id, model_filename);
    println!("🔍 Fetching model file: {}", model_url);
    
    match client.get(&model_url).header("x-api-key", &api_key).send().await {
        Ok(resp) if resp.status().is_success() => {
            match resp.text().await {
                Ok(model_json) => {
                    return (
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        model_json
                    ).into_response();
                }
                Err(e) => {
                    println!("⚠️ Failed to read model file: {}", e);
                    return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to read model: {}", e)).into_response();
                }
            }
        }
        Err(e) => {
            println!("⚠️ Failed to fetch model file: {}", e);
            return (StatusCode::BAD_GATEWAY, format!("Failed to fetch model: {}", e)).into_response();
        }
        Ok(r) => {
            println!("⚠️ Model file fetch failed: {} - {}", r.status(), model_url);
            return (StatusCode::NOT_FOUND, format!("Model file not found: {}", model_filename)).into_response();
        }
    }
}

// --- CONTROLLER PROXY HANDLERS ---
pub async fn list_controllers_proxy(
    State(state): State<AppState>,
    user: CurrentUser,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::ReadView) {
        return e.into_response();
    }

    // 1. Get primary supervisor
    let target_sup = state.settings.services.supervisor_url.clone();
    let api_key = state.settings.services.supervisor_api_key.clone();

    let client = Client::new();
    let url = format!("{}/api/controllers", target_sup);
    
    match client.get(&url).header("x-api-key", api_key).send().await {
        Ok(resp) => {
            let status = resp.status();
            match resp.text().await {
                Ok(body) => (status, body).into_response(),
                Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
            }
        }
        Err(e) => (StatusCode::BAD_GATEWAY, format!("Supervisor unreachable: {}", e)).into_response(),
    }
}



#[derive(Deserialize)]
pub struct LogTailQuery {
    pub lines: Option<usize>,
}

pub async fn get_controller_logs_proxy(
    AxumPath(id): AxumPath<String>,
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<LogTailQuery>,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::ReadView) {
        return e.into_response();
    }

    let target_sup = state.settings.services.supervisor_url.clone();
    let api_key = state.settings.services.supervisor_api_key.clone();

    let lines = query.lines.unwrap_or(80);
    let client = Client::new();
    let url = format!("{}/api/controllers/{}/logs/tail?lines={}", target_sup, id, lines);

    match client.get(&url).header("x-api-key", api_key).send().await {
        Ok(resp) => {
            let status = resp.status();
            match resp.text().await {
                Ok(body) => (status, body).into_response(),
                Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
            }
        }
        Err(e) => (StatusCode::BAD_GATEWAY, format!("Supervisor unreachable: {}", e)).into_response(),
    }
}

// --- QUERY HISTORICAL DATA FROM QUESTDB ---
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrendsQuery {
    nodes: String,       // JSON array of exact physical node IDs
    settings: Option<String>, // Optional JSON array of exact setting node IDs
    start: String,       // ISO timestamp or relative: "2024-01-10T14:00:00Z" or "-1h"
    end: Option<String>, // ISO timestamp or "now"
    max_points: Option<usize>,
}


fn parse_trend_node_ids(encoded: &str, field: &str, allow_empty: bool) -> Result<Vec<String>, String> {
    let nodes: Vec<String> = serde_json::from_str(encoded)
        .map_err(|_| format!("{field} must be a JSON array of node ID strings"))?;
    if !allow_empty && nodes.is_empty() {
        return Err(format!("{field} must contain at least one node ID"));
    }
    if nodes.iter().any(|node| node.trim().is_empty() || node.chars().any(char::is_control)) {
        return Err(format!("{field} contains an empty or control-character node ID"));
    }
    Ok(nodes)
}

pub async fn get_trends_data(
    Query(params): Query<TrendsQuery>,
    State(state): State<AppState>,
    user: CurrentUser,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::ReadView) {
        return e.into_response();
    }
    if let Err(error)=crate::runtime::authorize_legacy_read(&state,&user){return error.into_response();}


    // Parse end time - use provided end or default to now
    let end_time = if let Some(end_str) = &params.end {
        if end_str == "now" {
            chrono::Utc::now()
        } else {
            match chrono::DateTime::parse_from_rfc3339(end_str) {
                Ok(dt) => dt.with_timezone(&chrono::Utc),
                Err(_) => {
                    return (
                        axum::http::StatusCode::BAD_REQUEST,
                        "Invalid end time format (use ISO 8601 or 'now')"
                    ).into_response();
                }
            }
        }
    } else {
        chrono::Utc::now()
    };

    // Parse start time - can be relative or absolute
    let start_time = if params.start.starts_with('-') {
        // Relative time like "-1h", "-15m"
        match parse_relative_time(&params.start, end_time) {
            Ok(t) => t,
            Err(e) => {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    format!("Invalid start time: {}", e)
                ).into_response();
            }
        }
    } else {
        // Absolute ISO timestamp
        match chrono::DateTime::parse_from_rfc3339(&params.start) {
            Ok(dt) => dt.with_timezone(&chrono::Utc),
            Err(_) => {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    "Invalid start time format (use ISO 8601 or relative like '-1h')"
                ).into_response();
            }
        }
    };

    let node_ids = match parse_trend_node_ids(&params.nodes, "nodes", false) {
        Ok(nodes) => nodes,
        Err(error) => return (StatusCode::BAD_REQUEST, error).into_response(),
    };
    let setting_nodes = match parse_trend_node_ids(params.settings.as_deref().unwrap_or("[]"), "settings", true) {
        Ok(nodes) => nodes,
        Err(error) => return (StatusCode::BAD_REQUEST, error).into_response(),
    };

    match crate::process_trends::query(&state.historian_config, start_time, end_time, &node_ids, &setting_nodes, params.max_points).await {
        Ok(history) => Json(history).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            format!("無法讀取歷史趨勢：{error}"),
        ).into_response(),
    }
}

/// Parse relative time strings like "1h", "30m", "1d" into absolute DateTime
fn parse_relative_time(time_str: &str, reference: chrono::DateTime<chrono::Utc>) -> Result<chrono::DateTime<chrono::Utc>, String> {
    use chrono::Duration;
    
    // Try parsing as ISO 8601 first
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(time_str) {
        return Ok(dt.with_timezone(&chrono::Utc));
    }
    
    // Parse relative time (e.g., "1h", "30m", "2d")
    if time_str.len() < 2 {
        return Err("Time string too short".to_string());
    }
    
    let unit = time_str.chars().last().unwrap();
    let value_str = &time_str[..time_str.len()-1];
    let value: i64 = value_str.parse().map_err(|_| "Invalid time value")?;
    
    let duration = match unit {
        'h' => Duration::hours(value),
        'm' => Duration::minutes(value),
        's' => Duration::seconds(value),
        'd' => Duration::days(value),
        _ => return Err(format!("Unknown time unit: {}", unit)),
    };
    
    Ok(reference - duration)
}

// --- STEP RESPONSE API ROUTES ---

/// Request parameters for historical data query
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoricalDataQuery {
    pub start: String,  // ISO 8601 datetime
    pub end: String,    // ISO 8601 datetime
    pub nodes: String,  // JSON array of exact physical node IDs
}

/// Get historical time-series data for step response analysis
pub async fn get_step_response_data(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(params): Query<HistoricalDataQuery>,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::ReadView) {
        return e.into_response();
    }
    if let Err(error)=crate::runtime::authorize_legacy_read(&state,&user){return error.into_response();}

    use chrono::DateTime;
    use crate::historian::{query_historical_data, HistoricalQueryOptions};

    // Parse timestamps
    let start_time = match DateTime::parse_from_rfc3339(&params.start) {
        Ok(dt) => dt.with_timezone(&chrono::Utc),
        Err(_) => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                "Invalid start time format (use ISO 8601)"
            ).into_response();
        }
    };

    let end_time = match DateTime::parse_from_rfc3339(&params.end) {
        Ok(dt) => dt.with_timezone(&chrono::Utc),
        Err(_) => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                "Invalid end time format (use ISO 8601)"
            ).into_response();
        }
    };

    let node_ids = match parse_trend_node_ids(&params.nodes, "nodes", false) {
        Ok(nodes) => nodes,
        Err(error) => return (StatusCode::BAD_REQUEST, error).into_response(),
    };

    // Query historian
    match query_historical_data(
        &state.historian_config,
        start_time,
        end_time,
        node_ids,
        HistoricalQueryOptions::default(),
    ).await {
        Ok(data) => Json(data).into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to query historical data: {}", e)
        ).into_response()
    }
}

/// Get available tags for dropdown selection
pub async fn get_available_tags_api(
    State(state): State<AppState>,
    user: CurrentUser,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::ReadView) {
        return e.into_response();
    }
    if let Err(error)=crate::runtime::authorize_legacy_read(&state,&user){return error.into_response();}

    use crate::historian::get_available_tags;

    match get_available_tags(&state.historian_config).await {
        Ok(tags) => Json(tags).into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to get tags: {}", e)
        ).into_response()
    }
}

/// Step response calculation parameters
#[derive(Debug, Deserialize)]
pub struct StepResponseCalcRequest {
    pub mv_node: String,         // Which MV was stepped (e.g., "MV1:OP")
    pub cv_nodes: Vec<String>,   // Which CVs to model (e.g., ["CV1:PV", "CV2:PV"])
    pub mv_data: Vec<(String, f64)>,  // [(timestamp, value)]
    pub cv_data: Vec<Vec<(String, f64)>>,  // [[(timestamp, value)] for each CV]
    pub step_time: String,       // When the step occurred (ISO 8601)
    pub baseline_duration: f64,  // Seconds before step for baseline
    pub response_duration: f64,  // Seconds after step to capture
    pub sample_time: f64,        // Controller execution time in seconds (e.g., 20s)
}

/// Calculate step response coefficients from historical data
pub async fn calculate_step_response(
    State(_state): State<AppState>,
    user: CurrentUser,
    Json(request): Json<StepResponseCalcRequest>,
) -> impl IntoResponse {
    if let Err(e) = user.require(Permission::ModelDeploy) {
        return e.into_response();
    }

    use chrono::DateTime;

    // Parse step time
    let step_time = match DateTime::parse_from_rfc3339(&request.step_time) {
        Ok(dt) => dt.with_timezone(&chrono::Utc),
        Err(_) => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                "Invalid step time format"
            ).into_response();
        }
    };

    // Calculate baseline (average before step)
    let baseline_start = step_time - chrono::Duration::seconds(request.baseline_duration as i64);
    let (mv_baseline, baseline_count) = calculate_baseline_with_count(&request.mv_data, &baseline_start, &step_time);

    if baseline_count == 0 {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            format!("No data points found in baseline period ({}s before step). Try increasing baseline duration or check data availability.", request.baseline_duration)
        ).into_response();
    }

    // Find step size
    let (mv_after, after_count) = calculate_baseline_with_count(&request.mv_data, &step_time, 
        &(step_time + chrono::Duration::seconds(10)));  // First 10 seconds after step
    
    if after_count == 0 {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            "No data points found immediately after step time"
        ).into_response();
    }

    let step_size = mv_after - mv_baseline;

    if step_size.abs() < 0.01 {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            "Step size too small (< 0.01)"
        ).into_response();
    }

    // Calculate step response for each CV
    let num_points = (request.response_duration / request.sample_time).ceil() as usize;
    let mut responses = Vec::new();
    
    for (cv_node, cv_data) in request.cv_nodes.iter().zip(request.cv_data.iter()) {
        let cv_baseline = calculate_baseline(cv_data, &baseline_start, &step_time);
        
        // Collect all raw data points after step (within response duration)
        let mut raw_points: Vec<(f64, f64)> = Vec::new(); // (seconds_since_step, normalized_value)
        
        for (ts_str, value) in cv_data {
            if let Ok(ts) = DateTime::parse_from_rfc3339(ts_str) {
                let ts = ts.with_timezone(&chrono::Utc);
                if ts >= step_time {
                    let elapsed_secs = (ts - step_time).num_milliseconds() as f64 / 1000.0;
                    
                    if elapsed_secs <= request.response_duration {
                        let delta = (value - cv_baseline) / step_size;
                        raw_points.push((elapsed_secs, delta));
                    }
                }
            }
        }
        
        // Resample to uniform time grid using linear interpolation
        let coefficients = resample_step_response(&raw_points, num_points, request.sample_time);
        
        // Create fitted points with timestamps for visualization
        let fitted_points: Vec<(f64, f64)> = coefficients.iter().enumerate()
            .map(|(i, &val)| (i as f64 * request.sample_time, val))
            .collect();

        responses.push(serde_json::json!({
            "cv_node": cv_node,
            "cv_baseline": cv_baseline,
            "coefficients": coefficients,
            "raw_response": raw_points,  // For visualization
            "fitted_response": fitted_points,  // Resampled points
        }));
    }

    Json(serde_json::json!({
        "mv_node": request.mv_node,
        "mv_baseline": mv_baseline,
        "step_size": step_size,
        "step_time": request.step_time,
        "responses": responses,
    })).into_response()
}

/// Helper: Calculate baseline average with count
fn calculate_baseline_with_count(
    data: &[(String, f64)],
    start: &chrono::DateTime<chrono::Utc>,
    end: &chrono::DateTime<chrono::Utc>,
) -> (f64, usize) {
    use chrono::DateTime;
    
    let mut sum = 0.0;
    let mut count = 0;
    
    for (ts_str, value) in data {
        if let Ok(ts) = DateTime::parse_from_rfc3339(ts_str) {
            let ts = ts.with_timezone(&chrono::Utc);
            if ts >= *start && ts < *end {
                sum += value;
                count += 1;
            }
        }
    }
    
    let average = if count > 0 { sum / count as f64 } else { 0.0 };
    (average, count)
}

/// Helper: Calculate baseline average
fn calculate_baseline(
    data: &[(String, f64)],
    start: &chrono::DateTime<chrono::Utc>,
    end: &chrono::DateTime<chrono::Utc>,
) -> f64 {
    let (avg, _) = calculate_baseline_with_count(data, start, end);
    avg
}

/// Resample step response to uniform time grid with linear interpolation
fn resample_step_response(raw_points: &[(f64, f64)], num_points: usize, sample_time: f64) -> Vec<f64> {
    if raw_points.is_empty() {
        return vec![0.0; num_points];
    }
    
    let mut coefficients = Vec::with_capacity(num_points);
    
    for i in 0..num_points {
        let target_time = i as f64 * sample_time;
        
        // Find surrounding points for interpolation
        let mut before_idx = None;
        let mut after_idx = None;
        
        for (idx, &(t, _)) in raw_points.iter().enumerate() {
            if t <= target_time {
                before_idx = Some(idx);
            }
            if t >= target_time && after_idx.is_none() {
                after_idx = Some(idx);
                break;
            }
        }
        
        let value = match (before_idx, after_idx) {
            (Some(b), Some(a)) if b == a => {
                // Exact match
                raw_points[b].1
            }
            (Some(b), Some(a)) => {
                // Linear interpolation
                let (t1, v1) = raw_points[b];
                let (t2, v2) = raw_points[a];
                let ratio = (target_time - t1) / (t2 - t1);
                v1 + ratio * (v2 - v1)
            }
            (Some(b), None) => {
                // Extrapolate using last point (flat)
                raw_points[b].1
            }
            (None, Some(a)) => {
                // Before first point, use first value
                raw_points[a].1
            }
            (None, None) => 0.0,
        };
        
        coefficients.push(value);
    }
    
    coefficients
}


#[cfg(test)]
mod model_source_tests {
    use super::*;

    #[test]
    fn model_selection_cannot_change_the_configured_source_or_escape_its_route() {
        let source = ConfiguredModelSource {
            nodes_url: reqwest::Url::parse("http://configured.example:4841/api/nodes").unwrap(),
            endpoint: "opc.tcp://configured.example:4840".into(),
        };
        assert_eq!(model_nodes_url(&source, "cement-q1.yaml").unwrap().as_str(),
            "http://configured.example:4841/api/nodes/cement-q1.yaml");
        for filename in ["", "..", "../other", "http://attacker.example/map", "other\\map"] {
            assert!(model_nodes_url(&source, filename).is_err());
        }
        let encoded = model_nodes_url(&source, "%2e%2e?url=attacker").unwrap();
        assert_eq!(encoded.host_str(), Some("configured.example"));
        assert_eq!(encoded.path(), "/api/nodes/%252e%252e%3Furl=attacker");
        assert!(encoded.query().is_none());
    }
}

#[cfg(test)]
mod trend_query_tests {
    use super::*;

    #[test]
    fn exact_node_arrays_preserve_delimiters_case_and_whitespace_without_aliases() {
        assert_eq!(parse_trend_node_ids(r#"["Sensor","zone:loop:pv","Plant,Feed"," MixedCase "]"#, "nodes", false).unwrap(),
            ["Sensor", "zone:loop:pv", "Plant,Feed", " MixedCase "]);
        assert_eq!(parse_trend_node_ids(r#"["custom.request","custom.mode"]"#, "settings", true).unwrap(),
            ["custom.request", "custom.mode"]);
        assert!(parse_trend_node_ids("[]", "settings", true).unwrap().is_empty());
        assert!(parse_trend_node_ids("[]", "nodes", false).is_err());
    }

    #[test]
    fn malformed_or_legacy_trend_selections_are_rejected() {
        for encoded in ["Sensor,Feed", r#""Sensor""#, "null", r#"["Sensor", 1]"#, r#"[""]"#,
            r#"["  "]"#, r#"["line\nbreak"]"#, r#"{"node":"Sensor"}"#] {
            assert!(parse_trend_node_ids(encoded, "nodes", false).is_err(), "{encoded}");
        }
        assert!(serde_json::from_value::<TrendsQuery>(serde_json::json!({
            "tags": "Sensor:PV", "start": "-1h"
        })).is_err());
    }
}
