//! Authenticated transport for the local research CLI; Python owns datasets and run state.
use std::{io, path::Path, process::Stdio, sync::Arc, time::Duration};

use anyhow::{bail, Context};
use axum::{
    extract::{rejection::JsonRejection, DefaultBodyLimit, FromRequestParts, Path as RoutePath, Query, State},
    http::{header, request::Parts, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::{oneshot, Semaphore},
};

use crate::{
    auth::{AuthError, CsrfVerified, CurrentUser, Permission},
    config::{AppSettings, ResearchConfig},
    state::AppState,
};


#[path = "research_twin.rs"]
mod twin;
const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;
const MAX_STDOUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_STDERR_BYTES: usize = 64 * 1024;
const WORKER_CAPACITY: usize = 1;
pub(crate) type ResearchResult<T> = Result<T, ResearchError>;
type RequestJson = Result<Json<Value>, JsonRejection>;

#[derive(Debug)]
pub struct ResearchError(pub(crate) StatusCode, pub(crate) String);

impl IntoResponse for ResearchError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

pub(crate) fn invalid(message: impl Into<String>) -> ResearchError {
    ResearchError(StatusCode::BAD_REQUEST, message.into())
}

fn failure(message: impl Into<String>) -> ResearchError {
    ResearchError(StatusCode::INTERNAL_SERVER_ERROR, message.into())
}

/// Authorize before parsing large CSV bodies, with the same session revocation checks as the HMI.
pub struct ResearchUser(CurrentUser);

#[axum::async_trait]
impl FromRequestParts<AppState> for ResearchUser {
    type Rejection = ResearchError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ResearchResult<Self> {
        let user = CurrentUser::from_request_parts(parts, state).await.map_err(|error| match error {
            AuthError::Unauthorized => ResearchError(StatusCode::UNAUTHORIZED, "Sign in to use local Autoresearch.".into()),
            AuthError::Forbidden => ResearchError(StatusCode::FORBIDDEN, "Autoresearch requires an engineer or administrator account.".into()),
            _ => failure("Unable to validate the login session."),
        })?;
        if user.require(Permission::ResearchManage).is_err() {
            state.auth.audit(Some(&user), "research.access", None, "denied", None).await;
            return Err(ResearchError(StatusCode::FORBIDDEN, "Autoresearch requires an engineer or administrator account.".into()));
        }
        Ok(Self(user))
    }
}

pub struct ResearchStore {
    config: ResearchConfig,
    runtime_json: String,
    active: Arc<Semaphore>,
    twins: twin::Sessions,
}

impl ResearchStore {
    pub fn new(settings: &AppSettings) -> anyhow::Result<Self> {
        let config = settings.research.clone();
        crate::research_postgres::validate_sources(&config.postgres_sources)?;
        if config.enabled {
            if settings.runtime.connect_opcua || settings.historian.enabled {
                bail!("Local research requires runtime.connect_opcua=false and historian.enabled=false; use an isolated HMI configuration");
            }
            for (name, value) in [
                ("directory", &config.directory), ("python", &config.python),
                ("script", &config.script), ("binary", &config.binary), ("bridge", &config.bridge),
            ] {
                if !Path::new(value).is_absolute() {
                    bail!("research.{name} must be an absolute administrator-configured path");
                }
                if name != "directory" && !Path::new(value).is_file() {
                    bail!("research.{name} must identify an existing file");
                }
            }
            if !(1..=600).contains(&config.command_timeout_seconds) {
                bail!("research.command_timeout_seconds must be between 1 and 600");
            }
            if !(1..=86400).contains(&config.native_timeout_seconds) {
                bail!("research.native_timeout_seconds must be between 1 and 86400");
            }
        }
        let runtime_json = serde_json::to_string(&json!({
            "binary": config.binary,
            "bridge": config.bridge,
            "bridge_args": config.bridge_args,
            "local_only": true,
            "native_timeout_seconds": config.native_timeout_seconds,
        })).context("Cannot encode local research runtime")?;
        Ok(Self { config, runtime_json, active: Arc::new(Semaphore::new(WORKER_CAPACITY)), twins: twin::Sessions::new() })
    }

    /// Configuration and local permit state only: no worker launch or connection.
    pub(crate) fn maintenance_limits(&self) -> Value {
        json!({
            "isolated_research_enabled": self.config.enabled,
            "max_request_bytes": MAX_REQUEST_BYTES,
            "max_stdout_bytes": MAX_STDOUT_BYTES,
            "max_stderr_bytes": MAX_STDERR_BYTES,
            "command_timeout_seconds": self.config.command_timeout_seconds,
            "native_timeout_seconds": self.config.native_timeout_seconds,
            "worker_capacity": WORKER_CAPACITY,
            "available_worker_permits": self.active.available_permits(),
            "twin_limits": {
                "status": "not_exposed",
                "configured_limits": null
            }
        })
    }

    pub(crate) async fn version_bundle(&self, id: &str) -> ResearchResult<crate::control_versions::FrozenVersionBundle> {
        let value = self.execute("version-bundle", &json!({"id": id})).await?;
        let bundle: crate::control_versions::FrozenVersionBundle = serde_json::from_value(value)
            .map_err(|_| failure("The frozen version export has an invalid shape."))?;
        bundle.verify(id)?;
        Ok(bundle)
    }

    pub(crate) async fn publisher_candidates(&self, policy_id: &str, study_id: &str) -> ResearchResult<Vec<String>> {
        identifier(policy_id)?;
        identifier(study_id)?;
        let result = self.execute("publisher-candidates", &json!({"policy_id": policy_id, "study_id": study_id})).await?;
        serde_json::from_value(result["candidate_ids"].clone())
            .map_err(|_| failure("Immutable policy candidate listing has an invalid shape."))
    }

    pub(crate) async fn publisher_policies(&self) -> ResearchResult<Vec<Value>> {
        let result = self.execute("publisher-policies", &json!({})).await?;
        serde_json::from_value(result["policies"].clone())
            .map_err(|_| failure("Immutable publication policy listing has an invalid shape."))
    }

    fn require_enabled(&self) -> ResearchResult<()> {
        if !self.config.enabled {
            return Err(ResearchError(StatusCode::SERVICE_UNAVAILABLE,
                "Autoresearch is disabled. Ask an administrator to configure [research] in an isolated local HMI and set enabled=true.".into()));
        }
        Ok(())
    }

    fn spawn(&self, operation: &'static str) -> ResearchResult<Child> {
        // Read-only frozen artifacts remain available to a live HMI. This does
        // not enable import, cleaning, selection, or research execution there.
        if matches!(operation, "list-versions" | "get-version" | "version-model" | "version-bundle" | "publisher-candidates" | "publisher-policies") {
            for (name, value) in [
                ("directory", &self.config.directory), ("python", &self.config.python),
                ("script", &self.config.script),
            ] {
                if !Path::new(value).is_absolute() || (name != "directory" && !Path::new(value).is_file()) {
                    return Err(ResearchError(StatusCode::SERVICE_UNAVAILABLE,
                        format!("Configure an absolute research.{name} path to read frozen control versions.")));
                }
            }
            if !(1..=600).contains(&self.config.command_timeout_seconds) {
                return Err(failure("Invalid frozen-artifact command timeout."));
            }
        } else {
            self.require_enabled()?;
        }
        // No shell, user-supplied executable, connection endpoint or runtime environment.
        // Keep the administrator's isolated Wine environment for the local COM DA simulator.
        Command::new(&self.config.python)
            .arg("-u").arg(&self.config.script)
            .arg("--root").arg(&self.config.directory)
            .arg("--operation").arg(operation)
            .env("AUTHRITY_RESEARCH_RUNTIME", &self.runtime_json)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                eprintln!("Research {operation} could not start: {error}");
                failure("Cannot start the local research CLI. Ask an administrator to check the configured Python executable and script.")
            })
    }

    async fn execute(&self, operation: &'static str, payload: &Value) -> ResearchResult<Value> {
        let mut child = self.spawn(operation)?;
        if operation.starts_with("twin-") {
            let mut payload = payload.clone();
            payload["owner"] = json!(self.twins.owner);
            finish_process(&mut child, &payload, Some(Duration::from_secs(self.config.command_timeout_seconds))).await
        } else {
            finish_process(&mut child, payload, Some(Duration::from_secs(self.config.command_timeout_seconds))).await
        }
    }

    async fn mark_failed(&self, id: &str, error: &str) -> ResearchResult<()> {
        self.execute("fail-run", &json!({"id": id, "error": error})).await?;
        Ok(())
    }
}

struct Captured {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn capture(mut stream: impl AsyncRead + Unpin, limit: usize) -> io::Result<Captured> {
    let mut bytes = Vec::with_capacity(8192.min(limit));
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        let count = stream.read(&mut buffer).await?;
        if count == 0 { break; }
        let retained = count.min(limit - bytes.len());
        bytes.extend_from_slice(&buffer[..retained]);
        truncated |= retained != count;
        // Continue draining even at the limit; never block a child on a full pipe.
    }
    Ok(Captured { bytes, truncated })
}

async fn finish_process(child: &mut Child, payload: &Value, timeout: Option<Duration>) -> ResearchResult<Value> {
    let input = serde_json::to_vec(payload).map_err(|_| invalid("Request could not be encoded as JSON"))?;
    let mut stdin = child.stdin.take().ok_or_else(|| failure("Research CLI input is unavailable"))?;
    let stdout = child.stdout.take().ok_or_else(|| failure("Research CLI output is unavailable"))?;
    let stderr = child.stderr.take().ok_or_else(|| failure("Research CLI diagnostics are unavailable"))?;
    let communicate = async {
        tokio::try_join!(
            async move {
                stdin.write_all(&input).await?;
                stdin.shutdown().await?;
                drop(stdin); // The CLI reads stdin to EOF before dispatching.
                Ok::<(), io::Error>(())
            },
            capture(stdout, MAX_STDOUT_BYTES),
            capture(stderr, MAX_STDERR_BYTES),
            child.wait(),
        )
    };
    let result = if let Some(duration) = timeout {
        match tokio::time::timeout(duration, communicate).await {
            Ok(result) => result,
            Err(_) => {
                let _ = child.kill().await;
                return Err(ResearchError(StatusCode::GATEWAY_TIMEOUT,
                    "The local research command timed out. Refresh to check persisted state before retrying; ask an administrator to inspect worker logs.".into()));
            }
        }
    } else {
        communicate.await
    };
    let (_, stdout, stderr, status) = match result {
        Ok(output) => output,
        Err(error) => {
            let _ = child.kill().await;
            eprintln!("Research process communication failed: {error}");
            return Err(failure("Communication with the local research CLI failed. Inspect its persisted worker logs."));
        }
    };
    if !status.success() || stderr.truncated {
        eprintln!("Research CLI exit {status}; diagnostics{}: {}", if stderr.truncated { " (truncated)" } else { "" }, String::from_utf8_lossy(&stderr.bytes));
    }
    if stdout.truncated {
        return Err(failure("Research CLI response exceeded the 16 MiB limit; narrow the requested preview or series range."));
    }
    let parsed: Value = serde_json::from_slice(&stdout.bytes).map_err(|_| failure(
        "Research CLI returned no valid JSON response. Inspect its persisted worker logs and runtime configuration."))?;
    if !status.success() {
        return Err(ResearchError(StatusCode::UNPROCESSABLE_ENTITY,
            parsed.get("error").and_then(Value::as_str).unwrap_or("The local research command failed. Inspect worker logs.").to_owned()));
    }
    if !parsed.is_object() {
        return Err(failure("Research CLI returned an invalid response object"));
    }
    Ok(parsed)
}

fn request(payload: RequestJson, allowed: &[&str]) -> ResearchResult<Value> {
    let Json(payload) = payload.map_err(|error| ResearchError(error.status(), error.body_text()))?;
    let object = payload.as_object().ok_or_else(|| invalid("A JSON request object is required"))?;
    if let Some(field) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid(format!("Unsupported request field: {field}. Runtime paths and executables are administrator-configured only.")));
    }
    Ok(payload)
}

fn identifier(value: &str) -> ResearchResult<()> {
    if value.is_empty() || value.len() > 128 || !value.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_') {
        return Err(invalid("Invalid research identifier"));
    }
    Ok(())
}

async fn invoke(state: &AppState, user: &CurrentUser, operation: &'static str, payload: Value, audit: bool) -> ResearchResult<Json<Value>> {
    let result = state.research.execute(operation, &payload).await;
    if audit {
        let target = payload.get("id").or_else(|| payload.get("run_id")).or_else(|| payload.get("dataset_id")).or_else(|| payload.get("study_id")).and_then(Value::as_str);
        let detail = result.as_ref().err().map(|error| error.1.as_str());
        state.auth.audit(Some(user), &format!("research.{operation}"), target,
            if result.is_ok() { "success" } else { "failed" }, detail).await;
    }
    result.map(Json)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/research", get(bootstrap))
        .route("/api/research/import", post(import))
        .route("/api/research/sources", get(postgres_sources))
        .route("/api/research/import-postgres", post(import_postgres))
        .route("/api/research/models/:id", get(model_detail))
        .route("/api/research/import-snapshot", post(import_snapshot))
        .route("/api/research/identify", post(identify_model))
        .route("/api/research/models", get(models))
        .route("/api/research/policies", get(policies).post(save_policy))
        .route("/api/research/policies/:id/state", post(policy_state))
        .route("/api/research/drafts", get(drafts).post(save_draft))
        .route("/api/research/preview", post(preview))
        .route("/api/research/clean", post(clean))
        .route("/api/research/draft-study", post(draft_study))
        .route("/api/research/studies", post(save_study))
        .route("/api/research/versions", get(versions).post(select_version))
        .route("/api/research/versions/:id", get(version_detail))
        .route("/api/research/versions/:id/model", get(version_model))
        .route("/api/research/runs", post(start))
        .route("/api/research/runs/:id", get(detail))
        .route("/api/research/runs/:id/cancel", post(cancel))
        .route("/api/research/runs/:id/selection", post(selection))
        .route("/api/research/runs/:id/import-history", post(import_run_history))
        .route("/api/research/runs/:id/series", get(series))
        .route("/api/research/runs/:id/forecast", get(forecast))
        .merge(twin::routes())
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
}

async fn bootstrap(State(state): State<AppState>, ResearchUser(user): ResearchUser) -> ResearchResult<Json<Value>> {
    invoke(&state, &user, "bootstrap", json!({}), false).await
}

async fn drafts(State(state): State<AppState>, ResearchUser(user): ResearchUser) -> ResearchResult<Json<Value>> {
    invoke(&state, &user, "draft-list", json!({}), false).await
}

async fn save_draft(State(state): State<AppState>, ResearchUser(user): ResearchUser, _: CsrfVerified, body: RequestJson) -> ResearchResult<Json<Value>> {
    let mut payload = request(body, &["id", "expected_revision", "name", "dataset_id", "model_id", "config", "research"])?;
    payload["updated_by"] = json!(user.username);
    invoke(&state, &user, "draft-save", payload, true).await
}

async fn import_snapshot(State(state): State<AppState>, ResearchUser(user): ResearchUser, _: CsrfVerified, body: RequestJson) -> ResearchResult<Json<Value>> {
    let payload = request(body, &["name", "content", "manifest"])?;
    invoke(&state, &user, "import-snapshot", payload, true).await
}

async fn identify_model(State(state): State<AppState>, ResearchUser(user): ResearchUser, _: CsrfVerified, body: RequestJson) -> ResearchResult<Json<Value>> {
    let payload = request(body, &["study_id"])?;
    let permit = state.research.active.clone().try_acquire_owned()
        .map_err(|_| ResearchError(StatusCode::CONFLICT, "A research worker is active; qualification cannot compete for its resources.".into()))?;
    let (sender, receiver) = oneshot::channel();
    tokio::spawn(async move {
        let _permit = permit;
        let result = invoke(&state, &user, "identify-study", payload, true).await;
        let _ = sender.send(result);
    });
    receiver.await.map_err(|_| failure("The server-owned qualification task stopped before publishing its result."))?
}

async fn models(State(state): State<AppState>, ResearchUser(user): ResearchUser) -> ResearchResult<Json<Value>> {
    invoke(&state, &user, "list-models", json!({}), false).await
}

async fn model_detail(State(state): State<AppState>, ResearchUser(user): ResearchUser, RoutePath(id): RoutePath<String>) -> ResearchResult<Json<Value>> {
    identifier(&id)?;
    invoke(&state, &user, "get-model", json!({"id": id}), false).await
}

async fn policies(State(state): State<AppState>, ResearchUser(user): ResearchUser) -> ResearchResult<Json<Value>> {
    invoke(&state, &user, "policy-list", json!({}), false).await
}

async fn import_run_history(State(state): State<AppState>, ResearchUser(user): ResearchUser, _: CsrfVerified, RoutePath(id): RoutePath<String>, body: RequestJson) -> ResearchResult<Json<Value>> {
    identifier(&id)?;
    request(body, &[])?;
    invoke(&state, &user, "import-run-history", json!({"id": id}), true).await
}

async fn save_policy(State(state): State<AppState>, ResearchUser(user): ResearchUser, _: CsrfVerified, body: RequestJson) -> ResearchResult<Json<Value>> {
    let mut payload = request(body, &["id", "name", "study_id", "model_id", "action", "trigger", "enabled", "minimum_rows",
        "max_filled_fraction", "cooldown_seconds", "max_runs", "max_trials_total", "publication_template_version_id"])?;
    let realm = crate::runtime::publisher::identity_realm(&state.runtime).await
        .map_err(|_| failure("Cannot resolve the server author identity realm."))?;
    payload["created_by"] = json!({"user_id": user.id, "username": user.username, "identity_realm": realm});
    invoke(&state, &user, "policy-save", payload, true).await
}

async fn policy_state(State(state): State<AppState>, ResearchUser(user): ResearchUser, _: CsrfVerified, RoutePath(id): RoutePath<String>, body: RequestJson) -> ResearchResult<Json<Value>> {
    identifier(&id)?;
    let mut payload = request(body, &["enabled", "expected_revision"])?;
    payload["id"] = json!(id);
    invoke(&state, &user, "policy-state", payload, true).await
}

async fn selection(State(state): State<AppState>, ResearchUser(user): ResearchUser, _: CsrfVerified, RoutePath(id): RoutePath<String>, body: RequestJson) -> ResearchResult<Json<Value>> {
    identifier(&id)?;
    let mut payload = request(body, &["selections", "reason"])?;
    payload["id"] = json!(id);
    payload["selected_by"] = json!(user.username);
    invoke(&state, &user, "select-trials", payload, true).await
}

/// Register once at isolated-HMI startup. The browser never owns this clock.
pub fn start_policy_scheduler(state: AppState) {
    if !state.research.config.enabled {
        return;
    }
    tokio::spawn(async move {
        let mut clock = tokio::time::interval(Duration::from_secs(30));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            clock.tick().await;
            let permit = match state.research.active.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    if let Err(error) = state.research.execute("policy-heartbeat", &json!({})).await {
                        eprintln!("Research policy heartbeat failed: {}", error.1);
                    }
                    continue;
                }
            };
            if let Err(error) = state.research.execute("policy-reconcile", &json!({})).await {
                state.auth.audit(None, "research.policy-reconcile", None, "failed", Some(&error.1)).await;
                continue;
            }
            let decision = match state.research.execute("policy-tick", &json!({})).await {
                Ok(decision) => decision,
                Err(error) => {
                    state.auth.audit(None, "research.policy-tick", None, "failed", Some(&error.1)).await;
                    continue;
                }
            };
            let Some(id) = decision.get("run").and_then(|run| run.get("id")).and_then(Value::as_str)
                .filter(|id| identifier(id).is_ok()).map(str::to_owned) else { continue };
            let worker_state = state.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let result = match worker_state.research.spawn("run") {
                    Ok(mut child) => finish_process(&mut child, &json!({"id": id}), None).await,
                    Err(error) => Err(error),
                };
                let reason = result.as_ref().err().map(|error| error.1.as_str())
                    .unwrap_or("Policy worker exited without persisting a terminal status.");
                if let Err(error) = worker_state.research.mark_failed(&id, reason).await {
                    eprintln!("Cannot finalize policy run {id}: {}", error.1);
                }
                match worker_state.research.execute("policy-finalize", &json!({"id": id})).await {
                    Ok(sealed) => {
                        worker_state.auth.audit(None, "research.policy-candidate", Some(&id),
                            sealed["status"].as_str().unwrap_or("unknown"), sealed["reason"].as_str()).await;
                    }
                    Err(error) => {
                        worker_state.auth.audit(None, "research.policy-candidate", Some(&id), "failed", Some(&error.1)).await;
                    }
                }
                worker_state.auth.audit(None, "research.policy-worker", Some(&id),
                    if result.is_ok() { "success" } else { "failed" }, result.as_ref().err().map(|error| error.1.as_str())).await;
            });
        }
    });
}

async fn import(State(state): State<AppState>, ResearchUser(user): ResearchUser, _: CsrfVerified, body: RequestJson) -> ResearchResult<Json<Value>> {
    let payload = request(body, &["name", "format", "content"])?;
    invoke(&state, &user, "import", payload, true).await
}

async fn postgres_sources(State(state): State<AppState>, _: ResearchUser) -> ResearchResult<Json<Value>> {
    state.research.require_enabled()?;
    Ok(Json(json!({"sources": state.research.config.postgres_sources.iter()
        .map(crate::research_postgres::public_source).collect::<Vec<_>>()})))
}

async fn import_postgres(State(state): State<AppState>, ResearchUser(user): ResearchUser, _: CsrfVerified, body: RequestJson) -> ResearchResult<Json<Value>> {
    let payload = request(body, &["source_id", "name", "start", "end"])?;
    let source_id = payload.get("source_id").and_then(Value::as_str).map(str::to_owned);
    let result = async {
        let request = crate::research_postgres::parse_request(payload)?;
        state.research.require_enabled()?;
        let source = crate::research_postgres::find_source(&state.research.config.postgres_sources, &request.source_id)?;
        let payload = crate::research_postgres::import_payload(source, request).await?;
        state.research.execute("import", &payload).await
    }.await;
    // Driver diagnostics and connection configuration never enter audit details.
    state.auth.audit(Some(&user), "research.import-postgres", source_id.as_deref(),
        if result.is_ok() { "success" } else { "failed" }, result.as_ref().err().map(|error| error.1.as_str())).await;
    result.map(Json)
}

async fn preview(State(state): State<AppState>, ResearchUser(user): ResearchUser, _: CsrfVerified, body: RequestJson) -> ResearchResult<Json<Value>> {
    let payload = request(body, &["dataset_id", "cleaning"])?;
    invoke(&state, &user, "preview", payload, false).await
}

async fn clean(State(state): State<AppState>, ResearchUser(user): ResearchUser, _: CsrfVerified, body: RequestJson) -> ResearchResult<Json<Value>> {
    let payload = request(body, &["dataset_id", "cleaning"])?;
    invoke(&state, &user, "clean", payload, true).await
}

async fn draft_study(State(state): State<AppState>, ResearchUser(user): ResearchUser, _: CsrfVerified, body: RequestJson) -> ResearchResult<Json<Value>> {
    let payload = request(body, &["dataset_id"])?;
    invoke(&state, &user, "draft-study", payload, false).await
}

async fn save_study(State(state): State<AppState>, ResearchUser(user): ResearchUser, _: CsrfVerified, body: RequestJson) -> ResearchResult<Json<Value>> {
    let payload = request(body, &["id", "name", "dataset_id", "model_id", "cleaning", "config", "research"])?;
    invoke(&state, &user, "save-study", payload, true).await
}

async fn versions(State(state): State<AppState>, ResearchUser(user): ResearchUser) -> ResearchResult<Json<Value>> {
    invoke(&state, &user, "list-versions", json!({}), false).await
}

async fn select_version(State(state): State<AppState>, ResearchUser(user): ResearchUser, _: CsrfVerified, body: RequestJson) -> ResearchResult<Json<Value>> {
    let mut payload = request(body, &["run_id", "method", "deployment_profile_hash", "target", "equipment_instance_ids", "operating_envelope"])?;
    let run_id = payload.get("run_id").and_then(Value::as_str).ok_or_else(|| invalid("run_id is required"))?;
    identifier(run_id)?;
    let method = payload.get("method").and_then(Value::as_str).ok_or_else(|| invalid("method is required"))?;
    identifier(method)?;
    // Selection attribution comes from the authenticated session, never caller JSON.
    payload["selected_by"] = json!(user.username);
    payload["author_user_id"] = json!(user.id);
    payload["author_identity_realm"] = json!(crate::runtime::publisher::identity_realm(&state.runtime).await
        .map_err(|_| failure("Cannot resolve the server author identity realm."))?);
    invoke(&state, &user, "create-version", payload, true).await
}

async fn version_detail(State(state): State<AppState>, ResearchUser(user): ResearchUser, RoutePath(id): RoutePath<String>) -> ResearchResult<Json<Value>> {
    identifier(&id)?;
    invoke(&state, &user, "get-version", json!({"id": id}), false).await
}

async fn version_model(State(state): State<AppState>, _: ResearchUser, RoutePath(id): RoutePath<String>) -> ResearchResult<Response> {
    identifier(&id)?;
    let mut result = state.research.execute("version-model", &json!({"id": id})).await?;
    let model = match result.get_mut("model_json").map(Value::take) {
        Some(Value::String(model)) => model,
        _ => return Err(failure("The immutable model response is incomplete")),
    };
    let digest = match result.get_mut("model_sha256").map(Value::take) {
        Some(Value::String(digest)) if digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()) => digest,
        _ => return Err(failure("The immutable model digest is invalid")),
    };
    let disposition = HeaderValue::from_str(&format!("attachment; filename=\"control-model-{id}.json\""))
        .map_err(|_| failure("Cannot encode model download metadata"))?;
    let etag = HeaderValue::from_str(&format!("\"{digest}\""))
        .map_err(|_| failure("Cannot encode model digest"))?;
    // Do not serialize the parsed model again: the returned bytes must match the frozen digest.
    Ok(([(header::CONTENT_TYPE, HeaderValue::from_static("application/json")),
         (header::CONTENT_DISPOSITION, disposition), (header::ETAG, etag)], model).into_response())
}

async fn start(State(state): State<AppState>, ResearchUser(user): ResearchUser, _: CsrfVerified, body: RequestJson) -> ResearchResult<(StatusCode, Json<Value>)> {
    let payload = request(body, &["study_id"])?;
    let study_id = payload.get("study_id").and_then(Value::as_str).ok_or_else(|| invalid("study_id is required"))?;
    identifier(study_id)?;
    state.research.require_enabled()?;
    let permit = state.research.active.clone().try_acquire_owned().map_err(|_| ResearchError(
        StatusCode::CONFLICT, "A local research run is already active. Poll it or request cancellation before starting another.".into()))?;
    let (sender, receiver) = oneshot::channel();
    // The task owns both creation and execution: losing the browser cannot leave a
    // successfully created run queued without its worker. Python owns durable locking.
    tokio::spawn(async move {
        let _permit = permit;
        let result = state.research.execute("create-run", &payload).await;
        let run = match result {
            Ok(run) => run,
            Err(error) => {
                state.auth.audit(Some(&user), "research.create-run", payload["study_id"].as_str(), "failed", Some(&error.1)).await;
                let _ = sender.send(Err(error));
                return;
            }
        };
        let id = match run.get("id").and_then(Value::as_str).filter(|id| identifier(id).is_ok()) {
            Some(id) => id.to_owned(),
            None => {
                let _ = sender.send(Err(failure("The local CLI created a run without a valid identifier; inspect its persisted state.")));
                return;
            }
        };
        let mut child = match state.research.spawn("run") {
            Ok(child) => child,
            Err(error) => {
                if let Err(persist_error) = state.research.mark_failed(&id, &error.1).await {
                    eprintln!("Could not persist failed research run {id}: {}", persist_error.1);
                }
                state.auth.audit(Some(&user), "research.run", Some(&id), "failed", Some(&error.1)).await;
                let _ = sender.send(Err(error));
                return;
            }
        };
        state.auth.audit(Some(&user), "research.create-run", Some(&id), "success", None).await;
        let _ = sender.send(Ok(run));
        let result = finish_process(&mut child, &json!({"id": id}), None).await;
        // fail-run is a no-op for terminal runs. Also catches workers that exit zero
        // without persisting completion; a successful exit alone is not success.
        let reason = result.as_ref().err().map(|error| error.1.as_str())
            .unwrap_or("Research worker exited without persisting a terminal run status.");
        if let Err(error) = state.research.mark_failed(&id, reason).await {
            eprintln!("Could not finalize research run {id}: {}", error.1);
        }
        state.auth.audit(Some(&user), "research.worker-exit", Some(&id),
            if result.is_ok() { "success" } else { "failed" }, result.as_ref().err().map(|error| error.1.as_str())).await;
    });
    let run = receiver.await.map_err(|_| failure("Research launch task stopped unexpectedly. Refresh the run list to inspect persisted state."))??;
    Ok((StatusCode::ACCEPTED, Json(run)))
}

async fn detail(State(state): State<AppState>, ResearchUser(user): ResearchUser, RoutePath(id): RoutePath<String>) -> ResearchResult<Json<Value>> {
    identifier(&id)?;
    invoke(&state, &user, "detail", json!({"id": id}), false).await
}

async fn cancel(State(state): State<AppState>, ResearchUser(user): ResearchUser, _: CsrfVerified, RoutePath(id): RoutePath<String>) -> ResearchResult<Json<Value>> {
    identifier(&id)?;
    invoke(&state, &user, "cancel-run", json!({"id": id}), true).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SeriesQuery {
    method: String,
    variant: Option<String>,
    start: Option<f64>,
    end: Option<f64>,
    lead_steps: Option<u32>,
}

async fn series(State(state): State<AppState>, ResearchUser(user): ResearchUser, RoutePath(id): RoutePath<String>, Query(query): Query<SeriesQuery>) -> ResearchResult<Json<Value>> {
    identifier(&id)?;
    if query.start.into_iter().chain(query.end).any(|value| !value.is_finite() || value < 0.0)
        || matches!((query.start, query.end), (Some(start), Some(end)) if start > end)
        || matches!(query.lead_steps, Some(value) if value == 0 || value > 1_000_000)
    {
        return Err(invalid("Invalid chart range or lead_steps; use finite nonnegative times and a positive integer lead."));
    }
    invoke(&state, &user, "series", json!({"id": id, "method": query.method,
        "variant": query.variant.as_deref().unwrap_or("transport"), "start": query.start, "end": query.end,
        "lead_steps": query.lead_steps.unwrap_or(1)}), false).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ForecastQuery {
    method: String,
    variant: Option<String>,
    at: f64,
}

async fn forecast(State(state): State<AppState>, ResearchUser(user): ResearchUser, RoutePath(id): RoutePath<String>, Query(query): Query<ForecastQuery>) -> ResearchResult<Json<Value>> {
    identifier(&id)?;
    if !query.at.is_finite() || query.at < 0.0 {
        return Err(invalid("Forecast at must be a finite nonnegative relative time."));
    }
    invoke(&state, &user, "forecast", json!({"id": id, "method": query.method,
        "variant": query.variant.as_deref().unwrap_or("transport"), "at": query.at}), false).await
}

// Query validation occurs after authentication and before launching the worker.

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::{to_bytes, Body}, http::{header, Request}};
    use crate::{auth::{self, AuthService, Role}, simulation_reports::ReportStore};
    use tokio::sync::{broadcast, mpsc};
    use tower::ServiceExt;
    use tower_sessions::{MemoryStore, SessionManagerLayer};

    #[test]
    fn maintenance_limits_report_configuration_and_live_permits_without_worker_access() {
        let mut settings: AppSettings = toml::from_str(include_str!("../config/settings.toml")).unwrap();
        settings.research.enabled = false;
        settings.research.command_timeout_seconds = 47;
        settings.research.native_timeout_seconds = 901;
        let store = ResearchStore::new(&settings).unwrap();
        let limits = store.maintenance_limits();
        assert_eq!(limits["isolated_research_enabled"], false);
        assert_eq!(limits["max_request_bytes"], MAX_REQUEST_BYTES);
        assert_eq!(limits["max_stdout_bytes"], MAX_STDOUT_BYTES);
        assert_eq!(limits["max_stderr_bytes"], MAX_STDERR_BYTES);
        assert_eq!(limits["command_timeout_seconds"], 47);
        assert_eq!(limits["native_timeout_seconds"], 901);
        assert_eq!(limits["worker_capacity"], WORKER_CAPACITY);
        assert_eq!(limits["available_worker_permits"], WORKER_CAPACITY);
        assert!(limits["twin_limits"]["configured_limits"].is_null());
        let permit = store.active.try_acquire().unwrap();
        assert_eq!(store.maintenance_limits()["available_worker_permits"], 0);
        drop(permit);
        assert_eq!(store.maintenance_limits()["available_worker_permits"], WORKER_CAPACITY);
        assert!(limits.get("directory").is_none());
        assert!(limits.get("postgres_sources").is_none());
    }

    async fn application(role: Role) -> (Router, String, String) {
        let mut settings: AppSettings = toml::from_str(include_str!("../config/settings.toml")).unwrap();
        settings.research.enabled = false;
        settings.runtime.connect_opcua = false;
        settings.historian.enabled = false;
        let auth = AuthService::new("sqlite::memory:").await.unwrap();
        auth.create_user("research-test", "local-test-password", role).await.unwrap();
        let (tx, _) = broadcast::channel(1);
        let (cmd_tx, _) = mpsc::channel(1);
        let state = AppState {
            tx, cmd_tx,
            historian_config: settings.historian.clone(),
            research: Arc::new(ResearchStore::new(&settings).unwrap()),
            runtime: Arc::new(crate::runtime::RuntimeStore::open("sqlite::memory:", vec![], false).await.unwrap()),
            settings: Arc::new(settings),
            auth: Arc::new(auth),
            reports: Arc::new(ReportStore::new("unused-research-test-reports".into())),
        };
        let app = routes().merge(crate::control_versions::routes()).merge(crate::runtime::routes())
            .route("/api/auth/login", post(auth::login)).with_state(state)
            .layer(SessionManagerLayer::new(MemoryStore::default()).with_secure(false));
        let response = app.clone().oneshot(Request::builder().method("POST").uri("/api/auth/login")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"username":"research-test","password":"local-test-password"}"#)).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_owned();
        let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap();
        (app, cookie, body["csrf_token"].as_str().unwrap().to_owned())
    }

    #[tokio::test]
    async fn every_research_route_requires_login() {
        let (app, _, _) = application(Role::Engineer).await;
        for (method, uri) in [
            ("GET", "/api/research"), ("POST", "/api/research/import"),
            ("GET", "/api/research/sources"), ("POST", "/api/research/import-postgres"),
            ("POST", "/api/research/import-snapshot"), ("POST", "/api/research/identify"),
            ("GET", "/api/research/models"), ("GET", "/api/research/policies"), ("POST", "/api/research/policies"),
            ("POST", "/api/research/policies/policy-1/state"), ("POST", "/api/research/runs/run-1/selection"),
            ("GET", "/api/research/models/model-1"), ("POST", "/api/research/runs/run-1/import-history"),
            ("GET", "/api/research/drafts"), ("POST", "/api/research/drafts"),
            ("POST", "/api/research/preview"), ("POST", "/api/research/clean"),
            ("POST", "/api/research/draft-study"),
            ("POST", "/api/research/studies"), ("POST", "/api/research/runs"),
            ("GET", "/api/research/versions"), ("POST", "/api/research/versions"),
            ("GET", "/api/research/versions/version-1"), ("GET", "/api/research/versions/version-1/model"),
            ("GET", "/api/runtime/profiles"), ("POST", "/api/control/versions/version-1/install"),
            ("POST", "/api/control/versions/version-1/reconcile"),
            ("GET", "/api/research/runs/run-1"), ("POST", "/api/research/runs/run-1/cancel"),
            ("GET", "/api/research/runs/run-1/series?method=standard_dmc"),
            ("GET", "/api/research/runs/run-1/forecast?method=standard_dmc&at=10"),
            ("GET", "/api/research/runs/run-1/twin"),
            ("GET", "/api/research/runs/run-1/trials/trial-1/series"),
            ("GET", "/api/research/runs/run-1/trials/trial-1/twin-config"),
            ("GET", "/api/research/runs/run-1/trials/trial-1/frame?sample=0"),
            ("GET", "/api/research/twin/sessions/session-1/frame?sample=0"),
            ("POST", "/api/research/twin/equipment-views"),
            ("POST", "/api/research/twin/settings"),
            ("POST", "/api/research/twin/sessions"),
            ("GET", "/api/research/twin/sessions/session-1"),
            ("POST", "/api/research/twin/sessions/session-1/start"),
            ("POST", "/api/research/twin/sessions/session-1/pause"),
            ("POST", "/api/research/twin/sessions/session-1/step"),
            ("GET", "/api/research/twin/sessions/session-1/operations/operation-1"),
            ("POST", "/api/research/twin/sessions/session-1/close"),
            ("GET", "/api/research/twin/sessions/session-1/series"),
        ] {
            let response = app.clone().oneshot(Request::builder().method(method).uri(uri).body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn invalid_chart_queries_are_rejected_before_worker_launch() {
        let (app, cookie, _) = application(Role::Engineer).await;
        for uri in [
            "/api/research/runs/run-1/series?method=standard_dmc&lead_steps=0",
            "/api/research/runs/run-1/series?method=standard_dmc&lead_steps=1.5",
            "/api/research/runs/run-1/series?method=standard_dmc&start=20&end=10",
            "/api/research/runs/run-1/series?method=standard_dmc&start=NaN",
            "/api/research/runs/run-1/forecast?method=standard_dmc&at=-1",
            "/api/research/runs/run-1/forecast?method=standard_dmc&at=NaN",
            "/api/research/runs/run-1/forecast?method=standard_dmc&at=10&path=/tmp/other",
            "/api/research/runs/run-1/trials/trial-1/series?start=20&end=10",
            "/api/research/twin/sessions/session-1/series?start=NaN",
            "/api/research/twin/sessions/session-1/series?path=/tmp/other",
            "/api/research/runs/run-1/trials/trial-1/frame?sample=-1",
            "/api/research/runs/run-1/trials/trial-1/frame?sample=1.5",
            "/api/research/twin/sessions/session-1/frame",
            "/api/research/twin/sessions/session-1/frame?sample=0&path=/tmp/other",
        ] {
            let response = app.clone().oneshot(Request::builder().uri(uri)
                .header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
        }
    }

    #[tokio::test]
    async fn viewers_and_operators_cannot_access_research() {
        for role in [Role::Viewer, Role::Operator] {
            let (app, cookie, csrf) = application(role).await;
            for (method, uri) in [("GET", "/api/research"), ("POST", "/api/research/runs"),
                ("GET", "/api/research/sources"), ("POST", "/api/research/import-postgres"),
                ("GET", "/api/research/versions"), ("POST", "/api/research/versions"),
                ("GET", "/api/research/versions/version-1/model"),
                ("GET", "/api/research/runs/run-1/twin"),
                ("GET", "/api/research/runs/run-1/trials/trial-1/twin-config"),
                ("GET", "/api/research/runs/run-1/trials/trial-1/frame?sample=0"),
                ("GET", "/api/research/twin/sessions/session-1/frame?sample=0"),
                ("POST", "/api/research/twin/equipment-views"),
                ("POST", "/api/research/twin/settings"),
                ("POST", "/api/research/twin/sessions"),
                ("POST", "/api/research/twin/sessions/session-1/start"),
                ("POST", "/api/research/twin/sessions/session-1/pause"),
                ("POST", "/api/research/twin/sessions/session-1/step"),
                ("POST", "/api/research/twin/sessions/session-1/close"),
                ("GET", "/api/research/twin/sessions/session-1/operations/operation-1"),
                ("GET", "/api/research/runs/run-1/series?method=standard_dmc"),
                ("GET", "/api/research/runs/run-1/forecast?method=standard_dmc&at=10")] {
                let response = app.clone().oneshot(Request::builder().method(method).uri(uri)
                    .header(header::COOKIE, &cookie).header("x-csrf-token", &csrf)
                    .header(header::CONTENT_TYPE, "application/json").body(Body::from(r#"{"study_id":"study-1"}"#)).unwrap()).await.unwrap();
                assert_eq!(response.status(), StatusCode::FORBIDDEN, "{role:?}: {method} {uri}");
            }
            for action in ["install", "reconcile"] {
                let uri = format!("/api/control/versions/{}/{action}", "a".repeat(64));
                let response = app.clone().oneshot(Request::builder().method("POST").uri(&uri)
                    .header(header::COOKIE, &cookie).header("x-csrf-token", &csrf)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(installation_approval().to_string())).unwrap()).await.unwrap();
                assert_eq!(response.status(), StatusCode::FORBIDDEN, "{role:?}: {uri}");
            }
            // Runtime target discovery is scoped viewing, not research access.
            let response = app.oneshot(Request::builder().uri("/api/runtime/profiles")
                .header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap();
            assert_eq!(body["profiles"], json!([]));
        }
    }

    #[tokio::test]
    async fn engineers_and_admins_can_reach_disabled_research() {
        for role in [Role::Engineer, Role::Admin] {
            let (app, cookie, _) = application(role).await;
            for uri in ["/api/research", "/api/research/sources"] {
                let response = app.clone().oneshot(Request::builder().uri(uri).header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await.unwrap();
                assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            }
        }
    }

    #[tokio::test]
    async fn mutations_require_csrf_and_reject_runtime_overrides_and_bad_ids() {
        let (app, cookie, csrf) = application(Role::Engineer).await;
        let response = app.clone().oneshot(Request::builder().method("POST").uri("/api/research/runs")
            .header(header::COOKIE, &cookie).header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"study_id":"study-1"}"#)).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        for payload in [r#"{"study_id":"study-1","python":"/tmp/attacker"}"#, r#"{"study_id":"../outside"}"#, "[]"] {
            let response = app.clone().oneshot(Request::builder().method("POST").uri("/api/research/runs")
                .header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload)).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
    }

    #[tokio::test]
    async fn workbench_mutations_require_csrf_and_reject_external_authority() {
        let (app, cookie, csrf) = application(Role::Engineer).await;
        for uri in ["/api/research/import-snapshot", "/api/research/identify", "/api/research/drafts",
                    "/api/research/policies", "/api/research/policies/policy-1/state",
                    "/api/research/runs/run-1/selection", "/api/research/runs/run-1/import-history"] {
            let response = app.clone().oneshot(Request::builder().method("POST").uri(uri)
                .header(header::COOKIE, &cookie).header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}")).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{uri}");
            let response = app.clone().oneshot(Request::builder().method("POST").uri(uri)
                .header(header::COOKIE, &cookie).header("x-csrf-token", &csrf)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"dsn":"forbidden","binary":"/tmp/forbidden","deploy":true}"#)).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
        }
    }

    #[tokio::test]
    async fn twin_mutations_require_csrf_and_never_accept_runtime_metadata() {
        let (app, cookie, csrf) = application(Role::Engineer).await;
        for (uri, body) in [
            ("/api/research/twin/sessions", json!({"run_id":"run-1","trial_id":"trial-1","scenario":"selection","transport":"direct"})),
            ("/api/research/twin/sessions/session-1/start", json!({"operation_id":"start-1","batch_steps":1})),
            ("/api/research/twin/sessions/session-1/pause", json!({"operation_id":"pause-1"})),
            ("/api/research/twin/sessions/session-1/step", json!({"operation_id":"step-1"})),
            ("/api/research/twin/sessions/session-1/close", json!({"operation_id":"close-1"})),
            ("/api/research/twin/equipment-views", json!({"run_id":"run-1","trial_id":"trial-1","view":{}})),
            ("/api/research/twin/settings", json!({"run_id":"run-1","trial_id":"trial-1","name":"Test settings","variables":{}})),
        ] {
            let response = app.clone().oneshot(Request::builder().method("POST").uri(uri)
                .header(header::COOKIE, &cookie).header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string())).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{uri}");
            let response = app.clone().oneshot(Request::builder().method("POST").uri(uri)
                .header(header::COOKIE, &cookie).header("x-csrf-token", &csrf)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string())).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{uri}: valid request reaches disabled research");
            let mut forged = body;
            forged["binary"] = json!("/tmp/attacker");
            let response = app.clone().oneshot(Request::builder().method("POST").uri(uri)
                .header(header::COOKIE, &cookie).header("x-csrf-token", &csrf)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(forged.to_string())).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
            let error: Value = serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap();
            assert_eq!(error["error"], "Unsupported request field: binary. Runtime paths and executables are administrator-configured only.", "{uri}");
        }
    }

    fn installation_approval() -> Value {
        json!({
            "operation_id":"install-1",
            "profile_hash":"b".repeat(64),
            "approval_id":"approval-1",
            "expected_epoch":0,
            "expires_at":chrono::Utc::now() + chrono::Duration::seconds(30),
            "reason":"Install the independently approved version on its commissioned target"
        })
    }

    #[tokio::test]
    async fn installation_requires_csrf_explicit_target_and_server_owned_identity() {
        let (app, cookie, csrf) = application(Role::Engineer).await;
        let uri = format!("/api/control/versions/{}/install", "a".repeat(64));
        let body = installation_approval();
        let response = app.clone().oneshot(Request::builder().method("POST").uri(&uri)
            .header(header::COOKIE, &cookie).header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let response = app.clone().oneshot(Request::builder().method("POST").uri(&uri)
            .header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let error: Value = serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap();
        assert_eq!(error["error"], "Commissioned profile not found");
        for extra in ["approved_by", "model_json", "target_supervisor", "version_id", "expected_supervisor_url"] {
            let mut forged = body.clone();
            forged[extra] = json!("browser-controlled");
            let response = app.clone().oneshot(Request::builder().method("POST").uri(&uri)
                .header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(forged.to_string())).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
            let error = String::from_utf8(to_bytes(response.into_body(), 8192).await.unwrap().to_vec()).unwrap();
            assert!(error.contains(&format!("unknown field `{extra}`")), "{error}");
        }
        let mut missing_target = body.clone();
        missing_target.as_object_mut().unwrap().remove("profile_hash");
        let response = app.clone().oneshot(Request::builder().method("POST").uri(&uri)
            .header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(missing_target.to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let error = String::from_utf8(to_bytes(response.into_body(), 8192).await.unwrap().to_vec()).unwrap();
        assert!(error.contains("missing field `profile_hash`"), "{error}");
        let mut missing_reason = body;
        missing_reason["reason"] = json!(" ");
        let response = app.oneshot(Request::builder().method("POST").uri(&uri)
            .header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(missing_reason.to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn model_selection_rejects_forged_identity_authority_and_model_bytes() {
        let (app, cookie, csrf) = application(Role::Engineer).await;
        let response = app.clone().oneshot(Request::builder().method("POST").uri("/api/research/versions")
            .header(header::COOKIE, &cookie).header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"run_id":"run-1","method":"standard_dmc"}"#)).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        for payload in [
            json!({"run_id":"run-1", "method":"standard_dmc", "selected_by":"another-user"}),
            json!({"run_id":"run-1", "method":"standard_dmc", "authority":"factory"}),
            json!({"run_id":"run-1", "method":"standard_dmc", "model":{}}),
            json!({"run_id":"../outside", "method":"standard_dmc"}),
            json!({"run_id":"run-1", "method":"../outside"}),
        ] {
            let response = app.clone().oneshot(Request::builder().method("POST").uri("/api/research/versions")
                .header(header::COOKIE, &cookie).header("x-csrf-token", &csrf)
                .header(header::CONTENT_TYPE, "application/json").body(Body::from(payload.to_string())).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
    }

    #[tokio::test]
    async fn postgres_import_requires_csrf_and_rejects_bad_times_and_source_forgery() {
        let (app, cookie, csrf) = application(Role::Engineer).await;
        let valid = json!({"source_id":"kiln", "name":"Kiln data", "start":"2026-01-01T00:00:00Z", "end":"2026-01-02T00:00:00Z"});
        let response = app.clone().oneshot(Request::builder().method("POST").uri("/api/research/import-postgres")
            .header(header::COOKIE, &cookie).header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(valid.to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        for (uri, payload) in [
            ("/api/research/import-postgres", json!({"source_id":"kiln", "name":"Kiln data", "start":"2026-01-01T00:00:00", "end":"2026-01-02T00:00:00Z"})),
            ("/api/research/import-postgres", json!({"source_id":"kiln", "name":"Kiln data", "start":"2026-01-02T00:00:00Z", "end":"2026-01-01T00:00:00Z"})),
            ("/api/research/import-postgres", json!({"source_id":"kiln", "name":"Kiln data", "start":"2026-01-01T00:00:00Z", "end":"2026-01-02T00:00:00Z", "dsn":"postgres://browser"})),
            ("/api/research/import", json!({"name":"forged.csv", "format":"csv", "content":"time,pv\\n0,1\\n1,2", "source":{"kind":"postgres"}})),
        ] {
            let response = app.clone().oneshot(Request::builder().method("POST").uri(uri)
                .header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload.to_string())).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
        }
        let response = app.oneshot(Request::builder().method("POST").uri("/api/research/import-postgres")
            .header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(valid.to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn enabled_research_rejects_live_connections_even_with_valid_local_paths() {
        let mut settings: AppSettings = toml::from_str(include_str!("../config/settings.toml")).unwrap();
        settings.research.enabled = true;
        settings.runtime.connect_opcua = false;
        settings.historian.enabled = false;
        settings.research.directory = std::env::temp_dir().to_string_lossy().into_owned();
        let executable = std::env::current_exe().unwrap().to_string_lossy().into_owned();
        settings.research.python = executable.clone();
        settings.research.script = executable.clone();
        settings.research.binary = executable.clone();
        settings.research.bridge = executable;
        let _local = ResearchStore::new(&settings).expect("Local paths must be valid before checking connection isolation");
        for (opcua, historian) in [(true, false), (false, true), (true, true)] {
            settings.runtime.connect_opcua = opcua;
            settings.historian.enabled = historian;
            assert!(ResearchStore::new(&settings).is_err());
        }
    }

    #[tokio::test]
    async fn csv_import_accepts_bodies_larger_than_axums_default_limit() {
        let (app, cookie, csrf) = application(Role::Engineer).await;
        let payload = format!(r#"{{"name":"large.csv","format":"csv","content":"{}"}}"#, "x".repeat(3 * 1024 * 1024));
        let response = app.oneshot(Request::builder().method("POST").uri("/api/research/import")
            .header(header::COOKIE, cookie).header("x-csrf-token", csrf).header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload)).unwrap()).await.unwrap();
        // Request parsing succeeded; the disabled capability prevents any CLI launch.
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn csv_requests_have_an_explicit_size_limit() {
        let (app, cookie, csrf) = application(Role::Engineer).await;
        let payload = format!(r#"{{"name":"large.csv","format":"csv","content":"{}"}}"#, "x".repeat(MAX_REQUEST_BYTES));
        let response = app.oneshot(Request::builder().method("POST").uri("/api/research/import")
            .header(header::COOKIE, cookie).header("x-csrf-token", csrf).header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload)).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
