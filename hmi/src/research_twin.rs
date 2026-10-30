//! One serial owner per native JSONL session. HTTP cancellation never cancels a scan.
use super::*;
use std::{collections::HashMap, path::PathBuf};
use sha2::{Digest, Sha256};
use tokio::{io::{AsyncBufReadExt, BufReader}, process::{ChildStdin, ChildStdout}, sync::{mpsc, Mutex, OwnedSemaphorePermit}};

const MAX_ACTIVE: usize = 2;

pub(super) struct Sessions {
    pub(super) owner: String,
    active: Mutex<HashMap<String, mpsc::Sender<SessionRequest>>>,
    permits: Arc<Semaphore>,
}

impl Sessions {
    pub(super) fn new() -> Self {
        Self {
            owner: format!("{}-{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()),
            active: Mutex::new(HashMap::new()),
            permits: Arc::new(Semaphore::new(MAX_ACTIVE)),
        }
    }
}

impl ResearchStore {
    /// Run once before serving requests. Never respawn an orphan's native process.
    pub async fn reconcile_twins(&self) -> anyhow::Result<()> {
        if self.config.enabled {
            self.execute("twin-reconcile", &json!({})).await
                .map_err(|error| anyhow::anyhow!("Cannot reconcile twin sessions: {}", error.1))?;
        }
        Ok(())
    }
}

struct SessionRequest {
    command: Value,
    reply: oneshot::Sender<ResearchResult<Value>>,
}

struct Native {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    group: Option<u32>,
    stderr: Option<tokio::task::JoinHandle<io::Result<Captured>>>,
}

impl Drop for Native {
    fn drop(&mut self) {
        // kill_on_drop covers the child; kill the owned process group as well so
        // a blocked COM bridge cannot outlive its native owner on forced cleanup.
        self.kill_group();
        if let Some(task) = self.stderr.take() { task.abort(); }
    }
}

impl Native {
    fn kill_group(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.group.take() {
            // This ID comes only from our own process_group(0) spawn, never disk or HTTP.
            unsafe { libc::kill(-(pid as i32), libc::SIGKILL); }
        }
    }

    async fn stop(&mut self) {
        drop(self.stdin.take());
        if !matches!(tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await, Ok(Ok(_))) {
            self.kill_group();
            let _ = self.child.kill().await;
            let _ = self.child.wait().await;
        }
        // Never signal a reaped PID: its numeric process-group ID may be reused.
        self.group = None;
        if let Some(mut task) = self.stderr.take() {
            match tokio::time::timeout(Duration::from_secs(2), &mut task).await {
                Ok(Ok(Ok(captured))) if !captured.bytes.is_empty() =>
                    eprintln!("Twin native diagnostics{}: {}", if captured.truncated { " (truncated)" } else { "" }, String::from_utf8_lossy(&captured.bytes)),
                Err(_) => task.abort(),
                _ => {}
            }
        }
    }

    async fn exchange(&mut self, command: &Value) -> ResearchResult<Value> {
        let mut input = serde_json::to_vec(command).map_err(|_| invalid("Cannot encode native command"))?;
        input.push(b'\n');
        let stdin = self.stdin.as_mut().ok_or_else(|| failure("Native session input closed"))?;
        stdin.write_all(&input).await.map_err(|_| failure("Native session input closed"))?;
        stdin.flush().await.map_err(|_| failure("Cannot flush native command"))?;
        let mut line = Vec::with_capacity(8192);
        loop {
            let chunk = self.stdout.fill_buf().await.map_err(|_| failure("Cannot read native response"))?;
            if chunk.is_empty() { return Err(failure("Native session exited without a response")); }
            let newline = chunk.iter().position(|byte| *byte == b'\n');
            let count = newline.map_or(chunk.len(), |index| index + 1);
            if line.len() + count > MAX_STDOUT_BYTES { return Err(failure("Native response exceeded its resource limit")); }
            line.extend_from_slice(&chunk[..count]);
            self.stdout.consume(count);
            if newline.is_some() { break; }
        }
        let response: Value = serde_json::from_slice(&line).map_err(|_| failure("Native response is not JSON"))?;
        if response.get("request_id") != command.get("request_id") {
            return Err(failure("Native response request identity mismatch"));
        }
        Ok(response)
    }
}

async fn digest(path: &Path) -> ResearchResult<String> {
    let mut file = tokio::fs::File::open(path).await.map_err(|_| failure("Cannot read administrator-configured executable"))?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer).await.map_err(|_| failure("Cannot hash native executable"))?;
        if count == 0 { break; }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

async fn spawn_native(store: &ResearchStore, record: &Value) -> ResearchResult<Native> {
    let id = record["id"].as_str().ok_or_else(|| failure("Missing frozen session ID"))?;
    identifier(id)?;
    let directory = PathBuf::from(&store.config.directory).join("twin-sessions").join(id);
    if digest(Path::new(&store.config.binary)).await? != record["binary_sha256"].as_str().unwrap_or("") {
        return Err(failure("Native executable changed after freezing the session"));
    }
    if record["transport"] == "opc-da" && digest(Path::new(&store.config.bridge)).await? != record["bridge_sha256"].as_str().unwrap_or("") {
        return Err(failure("COM bridge changed after freezing the session"));
    }
    let mut command = Command::new(&store.config.binary);
    command.arg("session").arg("--plant").arg(directory.join("plant.json"))
        .arg("--controller").arg(directory.join("controller.json"))
        .arg("--scenario").arg(directory.join("scenario.json"))
        .arg("--output").arg(directory.join("result.json"))
        .arg("--forecast-output").arg(directory.join("forecast.ndjson"));
    if record["transport"] == "opc-da" {
        command.arg("--transport").arg("opc-da").arg("--bridge").arg(&store.config.bridge);
        for arg in &store.config.bridge_args { command.arg("--bridge-arg").arg(arg); }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
        #[cfg(target_os = "linux")]
        {
            let parent = std::process::id() as libc::pid_t;
            unsafe {
                command.as_std_mut().pre_exec(move || {
                    // Also clean up after abrupt HMI death, when Rust Drop cannot run.
                    // The native bridge installs the same Linux parent-death contract.
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::getppid() != parent { return Err(io::Error::new(io::ErrorKind::Interrupted, "HMI owner exited")); }
                    Ok(())
                });
            }
        }
    }
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = command.spawn().map_err(|_| failure("Cannot launch the configured native twin runner"))?;
    let group = child.id();
    let stdin = child.stdin.take().ok_or_else(|| failure("Native stdin is unavailable"))?;
    let stdout = BufReader::new(child.stdout.take().ok_or_else(|| failure("Native stdout is unavailable"))?);
    let stderr = child.stderr.take().ok_or_else(|| failure("Native stderr is unavailable"))?;
    Ok(Native { child, stdin: Some(stdin), stdout, group, stderr: Some(tokio::spawn(capture(stderr, MAX_STDERR_BYTES))) })
}

async fn persist_failure(store: &ResearchStore, record: &Value, reason: &str) -> ResearchResult<Value> {
    // A lost persistence reply may have committed newer samples already.
    let current = store.execute("twin-get", &json!({"id":record["id"]})).await?;
    if terminal(&current) { return Ok(current); }
    store.execute("twin-update", &json!({"id":current["id"], "response":{
        "status":"interrupted", "samples":current["samples"], "reason":reason}})).await
}

async fn exchange_owned(store: &ResearchStore, native: &mut Native, record: &mut Value,
                        mut command: Value, operation_id: Option<&str>) -> ResearchResult<()> {
    let mut intent = json!({"id":record["id"], "expected_sequence":record["sequence"], "command":command});
    if let Some(operation_id) = operation_id { intent["operation_id"] = json!(operation_id); }
    // Intent is fsync'd before native stdin. Neither a lost HTTP response nor an
    // ambiguous native response can re-issue this request.
    *record = store.execute("twin-native-begin", &intent).await?;
    command["request_id"] = record["pending_native"]["request_id"].clone();
    let response = match tokio::time::timeout(Duration::from_secs(store.config.command_timeout_seconds), native.exchange(&command)).await {
        Ok(result) => result?,
        Err(_) => return Err(ResearchError(StatusCode::GATEWAY_TIMEOUT,
            "Native command outcome is unknown. The session is interrupted; query the original operation, never replay it.".into())),
    };
    *record = store.execute("twin-update", &json!({"id":record["id"], "response":response})).await?;
    Ok(())
}

fn terminal(record: &Value) -> bool {
    matches!(record["status"].as_str(), Some("completed" | "failed" | "closed" | "interrupted"))
}

async fn session_owner(store: Arc<ResearchStore>, mut native: Native, mut record: Value,
                       mut receiver: mpsc::Receiver<SessionRequest>, _permit: OwnedSemaphorePermit,
                       ready: oneshot::Sender<ResearchResult<Value>>) {
    let id = record["id"].as_str().unwrap_or_default().to_owned();
    let initialized = exchange_owned(&store, &mut native, &mut record, json!({"command":"snapshot"}), None).await;
    if let Err(error) = initialized {
        native.stop().await;
        let _ = persist_failure(&store, &record, &error.1).await;
        let _ = ready.send(Err(error));
        store.twins.active.lock().await.remove(&id);
        return;
    }
    let _ = ready.send(Ok(record.clone()));
    while !terminal(&record) {
        // Commands win at each batch boundary. There is exactly one owner and
        // at most one accepted native batch, independent of browser lifetime.
        let request = if record["execution_status"] == "running" {
            match receiver.try_recv() {
                Ok(request) => Some(request),
                Err(mpsc::error::TryRecvError::Empty) => None,
                Err(mpsc::error::TryRecvError::Disconnected) => break,
            }
        } else {
            match receiver.recv().await {
                Some(request) => Some(request),
                None => break,
            }
        };
        if let Some(request) = request {
            let mut payload = request.command;
            payload["id"] = json!(id);
            let result = store.execute("twin-control", &payload).await;
            let control = match result {
                Ok(control) => control,
                Err(error) => {
                    // A CLI response can be lost after its atomic commit. Read
                    // that decision, never repeat the mutation or dispatch an
                    // accepted step whose acknowledgement was not received.
                    let reconciled = store.execute("twin-get", &json!({"id":id})).await;
                    let stop = match reconciled {
                        Ok(current) => {
                            let ambiguous = current["operation"]["id"] == payload["operation_id"]
                                && current["operation"]["status"] == "accepted";
                            record = current;
                            ambiguous
                        }
                        Err(_) => true,
                    };
                    if stop {
                        native.stop().await;
                        let _ = persist_failure(&store, &record, "Control intent could not be reconciled; no automatic replay").await;
                    }
                    let _ = request.reply.send(Err(error));
                    if stop { break; }
                    continue;
                }
            };
            record = control["session"].clone();
            if control["dispatch"] != true {
                let _ = request.reply.send(Ok(record.clone()));
                continue;
            }
            let operation_id = payload["operation_id"].as_str().unwrap_or_default();
            let command = if payload["command"] == "step" {
                json!({"command":"advance","expected_samples":record["samples"],"count":1})
            } else { json!({"command":"close"}) };
            match exchange_owned(&store, &mut native, &mut record, command, Some(operation_id)).await {
                Ok(()) => { let _ = request.reply.send(Ok(record.clone())); }
                Err(error) => {
                    native.stop().await;
                    let _ = persist_failure(&store, &record, &error.1).await;
                    let _ = request.reply.send(Err(error));
                    break;
                }
            }
        } else {
            let command = json!({"command":"advance", "expected_samples":record["samples"],
                "count":record["batch_steps"]});
            if let Err(error) = exchange_owned(&store, &mut native, &mut record, command, None).await {
                native.stop().await;
                let _ = persist_failure(&store, &record, &error.1).await;
                break;
            }
        }
    }
    native.stop().await;
    store.twins.active.lock().await.remove(&id);
    // Requests accepted while the final batch was running still resolve
    // against the durable ledger, rather than losing their operation identity.
    while let Ok(request) = receiver.try_recv() {
        let mut payload = request.command;
        payload["id"] = json!(id);
        let result = store.execute("twin-control", &payload).await.map(|value| value["session"].clone());
        let _ = request.reply.send(result);
    }
}

async fn command(store: &ResearchStore, id: &str, mut value: Value) -> ResearchResult<Value> {
    store.require_enabled()?;
    identifier(id)?;
    let sender = store.twins.active.lock().await.get(id).cloned();
    if let Some(sender) = sender {
        let (reply, response) = oneshot::channel();
        sender.try_send(SessionRequest { command: value, reply }).map_err(|_| ResearchError(StatusCode::CONFLICT,
            "The bounded session command queue is full; query the original operation before submitting anything else.".into()))?;
        response.await.map_err(|_| failure("Session owner exited; query the persisted operation"))?
    } else {
        let record = store.execute("twin-get", &json!({"id":id})).await?;
        if !terminal(&record) {
            // No in-memory owner means there is no checkpoint to continue.
            persist_failure(store, &record, "Native owner is unavailable; this session cannot resume or replay commands").await?;
        }
        value["id"] = json!(id);
        store.execute("twin-control", &value).await.map(|value| value["session"].clone())
    }
}

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/research/runs/:id/twin", get(manifest))
        .route("/api/research/runs/:id/trials/:trial_id/series", get(trial_series))
        .route("/api/research/runs/:id/trials/:trial_id/twin-config", get(configuration))
        .route("/api/research/runs/:id/trials/:trial_id/frame", get(trial_frame))
        .route("/api/research/twin/equipment-views", post(equipment_view))
        .route("/api/research/twin/settings", post(settings))
        .route("/api/research/twin/sessions", post(create))
        .route("/api/research/twin/sessions/:id", get(snapshot))
        .route("/api/research/twin/sessions/:id/start", post(start))
        .route("/api/research/twin/sessions/:id/pause", post(pause))
        .route("/api/research/twin/sessions/:id/step", post(step))
        .route("/api/research/twin/sessions/:id/operations/:operation_id", get(operation))
        .route("/api/research/twin/sessions/:id/close", post(close))
        .route("/api/research/twin/sessions/:id/series", get(session_series))
        .route("/api/research/twin/sessions/:id/frame", get(session_frame))
}

async fn manifest(State(state): State<AppState>, ResearchUser(user): ResearchUser, RoutePath(id): RoutePath<String>) -> ResearchResult<Json<Value>> {
    identifier(&id)?;
    invoke(&state, &user, "twin-manifest", json!({"id":id}), false).await
}

async fn create(State(state): State<AppState>, ResearchUser(user): ResearchUser, _: CsrfVerified, body: RequestJson) -> ResearchResult<Json<Value>> {
    let payload = request(body, &["run_id", "trial_id", "scenario", "transport", "equipment_view_id", "settings_version_id"])?;
    for key in ["equipment_view_id", "settings_version_id"] {
        if let Some(value) = payload.get(key) {
            let id = value.as_str().ok_or_else(|| invalid(format!("{key} must be a version ID")))?;
            if id.len() != 64 || !id.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) {
                return Err(invalid(format!("{key} must be a server-owned content digest")));
            }
        }
    }
    for key in ["run_id", "trial_id"] {
        identifier(payload[key].as_str().ok_or_else(|| invalid(format!("{key} is required")))?)?;
    }
    if !matches!(payload["scenario"].as_str(), Some("selection" | "full")) || !matches!(payload["transport"].as_str(), Some("direct" | "opc-da")) {
        return Err(invalid("Choose selection/full scenario and direct/opc-da transport"));
    }
    state.research.require_enabled()?;
    let permit = state.research.twins.permits.clone().try_acquire_owned().map_err(|_| ResearchError(StatusCode::CONFLICT,
        "At most two native twin sessions may be active. Close an existing session first.".into()))?;
    let (reply, response) = oneshot::channel();
    tokio::spawn(async move {
        // Creation, native start, first snapshot and persistence outlive a disconnected browser.
        let result = async {
            let record = state.research.execute("twin-create", &payload).await?;
            let id = record["id"].as_str().ok_or_else(|| failure("Frozen session ID missing"))?.to_owned();
            let native = match spawn_native(&state.research, &record).await {
                Ok(native) => native,
                Err(error) => {
                    let _ = persist_failure(&state.research, &record, &error.1).await;
                    return Err(error);
                }
            };
            let (sender, receiver) = mpsc::channel(32);
            let (ready, initialized) = oneshot::channel();
            state.research.twins.active.lock().await.insert(id.clone(), sender);
            tokio::spawn(session_owner(state.research.clone(), native, record, receiver, permit, ready));
            initialized.await.map_err(|_| failure("Native owner exited during initialization"))?
        }.await;
        state.auth.audit(Some(&user), "research.twin-create", payload["run_id"].as_str(),
            if result.is_ok() { "success" } else { "failed" }, result.as_ref().err().map(|error| error.1.as_str())).await;
        let _ = reply.send(result);
    });
    response.await.map_err(|_| failure("Twin creation owner exited; inspect the run manifest"))?.map(Json)
}

async fn snapshot(State(state): State<AppState>, _: ResearchUser, RoutePath(id): RoutePath<String>) -> ResearchResult<Json<Value>> {
    identifier(&id)?;
    state.research.execute("twin-get", &json!({"id":id})).await.map(Json)
}

async fn operation(State(state): State<AppState>, _: ResearchUser,
    RoutePath((id, operation_id)): RoutePath<(String, String)>) -> ResearchResult<Json<Value>> {
    identifier(&id)?;
    identifier(&operation_id)?;
    state.research.execute("twin-operation", &json!({"id":id,"operation_id":operation_id})).await.map(Json)
}

async fn mutate(state: AppState, user: CurrentUser, id: String, payload: Value) -> ResearchResult<Json<Value>> {
    identifier(&id)?;
    let (reply, response) = oneshot::channel();
    tokio::spawn(async move {
        let action = match payload["command"].as_str() {
            Some("start") => "research.twin-start",
            Some("pause") => "research.twin-pause",
            Some("step") => "research.twin-step",
            _ => "research.twin-close",
        };
        let operation_id = payload["operation_id"].as_str().unwrap_or_default().to_owned();
        let result = command(&state.research, &id, payload).await;
        let (outcome, reason) = match &result {
            Ok(value) => (value["operation"]["status"].as_str().unwrap_or("unknown"),
                          value["operation"]["reason"].as_str().unwrap_or("")),
            Err(error) => ("failed", error.1.as_str()),
        };
        let detail = format!("operation_id={operation_id}; {reason}");
        state.auth.audit(Some(&user), action, Some(&id), outcome, Some(&detail)).await;
        let _ = reply.send(result);
    });
    response.await.map_err(|_| failure("Twin command owner exited; fetch the persisted snapshot"))?.map(Json)
}

fn control_payload(body: RequestJson, command: &'static str) -> ResearchResult<Value> {
    let mut payload = request(body, if command == "start" { &["operation_id", "batch_steps"] } else { &["operation_id"] })?;
    let operation_id = payload["operation_id"].as_str().ok_or_else(|| invalid("A durable operation_id is required"))?;
    identifier(operation_id)?;
    if !operation_id.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric) {
        return Err(invalid("operation_id must begin with an ASCII letter or digit"));
    }
    if command == "start" {
        let count = payload.get("batch_steps").map_or(Some(128), Value::as_u64);
        if !matches!(count, Some(1..=512)) { return Err(invalid("batch_steps must be an integer 1..512")); }
        payload["batch_steps"] = json!(count.unwrap_or(128));
    }
    payload["command"] = json!(command);
    Ok(payload)
}

async fn start(State(state): State<AppState>, ResearchUser(user): ResearchUser, _: CsrfVerified, RoutePath(id): RoutePath<String>, body: RequestJson) -> ResearchResult<Json<Value>> {
    mutate(state, user, id, control_payload(body, "start")?).await
}

async fn pause(State(state): State<AppState>, ResearchUser(user): ResearchUser, _: CsrfVerified, RoutePath(id): RoutePath<String>, body: RequestJson) -> ResearchResult<Json<Value>> {
    mutate(state, user, id, control_payload(body, "pause")?).await
}

async fn step(State(state): State<AppState>, ResearchUser(user): ResearchUser, _: CsrfVerified, RoutePath(id): RoutePath<String>, body: RequestJson) -> ResearchResult<Json<Value>> {
    mutate(state, user, id, control_payload(body, "step")?).await
}

async fn close(State(state): State<AppState>, ResearchUser(user): ResearchUser, _: CsrfVerified, RoutePath(id): RoutePath<String>, body: RequestJson) -> ResearchResult<Json<Value>> {
    mutate(state, user, id, control_payload(body, "close")?).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Window { start: Option<f64>, end: Option<f64> }

impl Window {
    fn payload(&self, id: &str) -> ResearchResult<Value> {
        identifier(id)?;
        if self.start.into_iter().chain(self.end).any(|v| !v.is_finite() || v < 0.0)
            || matches!((self.start, self.end), (Some(a), Some(b)) if a > b) {
            return Err(invalid("Use finite nonnegative scenario-relative times with start <= end"));
        }
        Ok(json!({"id":id, "start":self.start, "end":self.end}))
    }
}

async fn trial_series(State(state): State<AppState>, ResearchUser(user): ResearchUser,
    RoutePath((id, trial_id)): RoutePath<(String, String)>, Query(window): Query<Window>) -> ResearchResult<Json<Value>> {
    identifier(&trial_id)?;
    let mut payload = window.payload(&id)?;
    payload["trial_id"] = json!(trial_id);
    invoke(&state, &user, "twin-trial-series", payload, false).await
}

async fn session_series(State(state): State<AppState>, ResearchUser(user): ResearchUser,
    RoutePath(id): RoutePath<String>, Query(window): Query<Window>) -> ResearchResult<Json<Value>> {
    invoke(&state, &user, "twin-session-series", window.payload(&id)?, false).await
}

async fn configuration(State(state): State<AppState>, ResearchUser(user): ResearchUser,
    RoutePath((id, trial_id)): RoutePath<(String, String)>) -> ResearchResult<Json<Value>> {
    identifier(&id)?;
    identifier(&trial_id)?;
    invoke(&state, &user, "twin-config", json!({"id":id,"trial_id":trial_id}), false).await
}

fn version_payload(body: RequestJson, settings: bool) -> ResearchResult<Value> {
    let payload = request(body, if settings { &["run_id", "trial_id", "name", "variables"] }
        else { &["run_id", "trial_id", "view"] })?;
    for key in ["run_id", "trial_id"] {
        identifier(payload[key].as_str().ok_or_else(|| invalid(format!("{key} is required")))?)?;
    }
    if settings {
        if !payload["name"].as_str().is_some_and(|name| !name.trim().is_empty() && name.len() <= 640)
            || !payload["variables"].is_object() {
            return Err(invalid("Settings require a name and full controller variables"));
        }
    } else if !payload["view"].is_object() {
        return Err(invalid("Equipment view is required"));
    }
    Ok(payload)
}

async fn equipment_view(State(state): State<AppState>, ResearchUser(user): ResearchUser,
    _: CsrfVerified, body: RequestJson) -> ResearchResult<Json<Value>> {
    invoke(&state, &user, "twin-equipment-view", version_payload(body, false)?, true).await
}

async fn settings(State(state): State<AppState>, ResearchUser(user): ResearchUser,
    _: CsrfVerified, body: RequestJson) -> ResearchResult<Json<Value>> {
    invoke(&state, &user, "twin-settings", version_payload(body, true)?, true).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FrameQuery { sample: usize }

async fn trial_frame(State(state): State<AppState>, ResearchUser(user): ResearchUser,
    RoutePath((id, trial_id)): RoutePath<(String, String)>, Query(query): Query<FrameQuery>) -> ResearchResult<Json<Value>> {
    identifier(&id)?;
    identifier(&trial_id)?;
    invoke(&state, &user, "twin-trial-frame", json!({"id":id,"trial_id":trial_id,"sample":query.sample}), false).await
}

async fn session_frame(State(state): State<AppState>, ResearchUser(user): ResearchUser,
    RoutePath(id): RoutePath<String>, Query(query): Query<FrameQuery>) -> ResearchResult<Json<Value>> {
    identifier(&id)?;
    invoke(&state, &user, "twin-session-frame", json!({"id":id,"sample":query.sample}), false).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_targets_preserve_binary64_at_the_json_boundary() {
        // Values from a real quarter scan. Frozen scenario checks require exact
        // native numbers, not a one-ULP change introduced by the JSON relay.
        let native = br#"{"frame":{"targets":[357.6071454725318,39.091724578010684]}}"#;
        let response: Value = serde_json::from_slice(native).unwrap();
        let expected = [357.6071454725318_f64, 39.091724578010684_f64];
        for (index, expected) in expected.into_iter().enumerate() {
            let actual = response["frame"]["targets"][index].as_f64().unwrap();
            assert_eq!(actual.to_bits(), expected.to_bits(), "native target {index} changed during JSON decoding");
        }
    }

    #[test]
    fn controls_require_operation_identity_and_bounded_integer_batches() {
        for value in [json!({}), json!({"operation_id":""}), json!({"operation_id":"../escape"}),
            json!({"operation_id":"one","batch_steps":0}), json!({"operation_id":"one","batch_steps":513}),
            json!({"operation_id":"one","batch_steps":1.5}), json!({"operation_id":"one","count":1})] {
            assert!(control_payload(Ok(Json(value)), "start").is_err());
        }
        assert!(control_payload(Ok(Json(json!({"operation_id":"one","batch_steps":512}))), "start").is_ok());
        assert!(control_payload(Ok(Json(json!({"operation_id":"one","batch_steps":1}))), "step").is_err());
    }

    #[test]
    fn frozen_chart_window_rejects_reversed_nonfinite_and_path_ids() {
        assert!(Window { start: Some(10.0), end: Some(1.0) }.payload("session-1").is_err());
        assert!(Window { start: Some(f64::NAN), end: None }.payload("session-1").is_err());
        assert!(Window { start: None, end: None }.payload("../result").is_err());
    }
}
