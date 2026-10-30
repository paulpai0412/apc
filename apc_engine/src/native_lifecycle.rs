//! Private supervisor IPC. OPC service acceptance is never lifecycle confirmation.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::{HashSet, HashMap}, path::Path, sync::{Arc, Mutex}, time::{Duration, Instant}, io::Write};
use tokio::{io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader}, net::UnixListener, sync::{mpsc, oneshot}};
use crate::config::UnifiedModel;

/// Restart identity must remain unique even across PID reuse and wall-clock rollback.
pub fn new_instance_id() -> Result<String> {
    use std::io::Read;
    use std::fmt::Write as _;
    let mut random = [0_u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
    random[6] = (random[6] & 0x0f) | 0x40;
    random[8] = (random[8] & 0x3f) | 0x80;
    let mut id = String::with_capacity(36);
    for (index, byte) in random.into_iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) { id.push('-'); }
        write!(&mut id, "{byte:02x}")?;
    }
    Ok(id)
}

#[derive(Clone, Deserialize)]
pub struct Variable {
    pub id: String, pub role: String, pub unit: String, pub pv_tag: String,
    pub command_tag: Option<String>, pub applied_tag: Option<String>,
    pub op_tag: Option<String>,
    pub mode_tag: Option<String>, pub mode_target_tag: Option<String>,
    pub target_tag: Option<String>, pub low_tag: Option<String>, pub high_tag: Option<String>,
    pub command_coordinate: Option<String>, pub feedback_coordinate: Option<String>,
    pub remote_mode: Option<f64>, pub hard_low: f64, pub hard_high: f64, pub max_move: Option<f64>,
}
#[derive(Clone, Deserialize)]
pub struct Handoff {
    pub release_tag: String, pub release_value: f64, pub acknowledgement_tag: String,
    pub acknowledgement_value: f64, pub timeout_ms: u64, pub watchdog_evidence: String,
    pub acquire_value: f64, pub watchdog_tag: String, pub watchdog_timeout_ms: u64, pub local_mode: f64,
}
#[derive(Clone, Deserialize)]
pub struct Commissioning {
    pub profile_hash: String, pub engine_id: String, pub model_sha256: String, pub endpoint: String,
    pub acquisition_sequence_node: String, pub history_capacity_samples: usize,
    pub maximum_age_ms: u64, pub variables: Vec<Variable>, pub handoff: Handoff,
    pub application_tolerance: f64,
    pub approved_by: String, pub commissioning_evidence: String,
}
impl Commissioning {
    pub fn load(path: &Path, model: &UnifiedModel, digest: &str, endpoint: &str, startup_approval: Option<&Path>) -> Result<Self> {
        ensure!(path.is_absolute(), "Commissioning path must be absolute");
        ensure!(std::fs::metadata(path)?.len() <= 4 * 1024 * 1024, "Commissioning file too large");
        let document: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
        let profile: Self = serde_json::from_value(document.clone())?;
        ensure!(profile.endpoint == endpoint, "Commissioned endpoint mismatch");
        match startup_approval {
            Some(approval) => crate::native_approval::load(approval, &document, digest)?,
            None => ensure!(profile.model_sha256 == digest, "Non-baseline startup requires independently approved immutable provenance"),
        }
        profile.validate_model(model)?;
        Ok(profile)
    }
    pub fn validate_model(&self, model: &UnifiedModel) -> Result<()> {
        ensure!([&self.profile_hash, &self.model_sha256].iter().all(|hash| hash.len() == 64
            && hash.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
            && !self.approved_by.trim().is_empty() && !self.commissioning_evidence.trim().is_empty(),
            "Unapproved commissioning identity");
        ensure!(!self.acquisition_sequence_node.is_empty() && self.maximum_age_ms > 0
            && self.maximum_age_ms <= 300_000 && (2..=65_536).contains(&self.history_capacity_samples), "Invalid acquisition/history contract");
        ensure!(self.application_tolerance.is_finite() && self.application_tolerance >= 0.0,
            "Explicit finite engineering-unit application tolerance required");
        let h = &self.handoff;
        ensure!(!h.release_tag.is_empty() && !h.acknowledgement_tag.is_empty() && h.release_tag != h.acknowledgement_tag
            && h.release_value.is_finite() && h.acknowledgement_value.is_finite() && h.timeout_ms > 0 && h.timeout_ms <= 300_000
            && !h.watchdog_evidence.trim().is_empty(), "Missing independent DCS release/watchdog contract");
        ensure!(h.acquire_value.is_finite() && h.acquire_value != h.release_value && h.local_mode.is_finite()
            && !h.watchdog_tag.is_empty() && h.watchdog_tag != h.release_tag && h.watchdog_tag != h.acknowledgement_tag
            && h.watchdog_timeout_ms > 0 && h.watchdog_timeout_ms <= 3_600_000,
            "Missing explicit acquisition/watchdog control contract");
        ensure!(model.metadata.name == self.engine_id, "Commissioned engine identity differs");
        let cvs: Vec<_> = self.variables.iter().filter(|v| v.role == "CV").collect();
        let mvs: Vec<_> = self.variables.iter().filter(|v| v.role == "MV").collect();
        let dvs: Vec<_> = self.variables.iter().filter(|v| v.role == "DV").collect();
        ensure!(cvs.len() == model.variables.cvs.len() && mvs.len() == model.variables.mvs.len()
            && dvs.len() == model.variables.dvs.len(), "Commissioned variable dimensions differ");
        ensure!(self.variables.iter().all(|v| matches!(v.role.as_str(), "CV" | "MV" | "DV"))
            && self.variables.iter().map(|v| &v.id).collect::<HashSet<_>>().len() == self.variables.len(),
            "Unknown or duplicated commissioned variable");
        let mut commands = HashSet::new();
        for (v, mv) in mvs.iter().zip(&model.variables.mvs) {
            ensure!(v.id == mv.name && v.unit == mv.units && v.command_tag.as_deref() == Some(mv.node_ids.sp.as_str())
                && v.applied_tag == mv.node_ids.applied_sp && v.applied_tag.is_some() && v.applied_tag != v.command_tag
                && v.mode_tag.as_deref() == Some(mv.node_ids.mode.as_str())
                && v.mode_target_tag.as_deref() == Some(mv.node_ids.mode_target.as_str())
                && v.pv_tag == mv.node_ids.pv
                && v.op_tag.as_deref() == Some(mv.node_ids.op.as_str())
                && v.target_tag == mv.node_ids.target
                && v.low_tag.as_deref() == Some(mv.node_ids.limits.low.as_str())
                && v.high_tag.as_deref() == Some(mv.node_ids.limits.high.as_str())
                && v.command_coordinate.as_deref().is_some_and(|s| !s.is_empty()) && v.command_coordinate == v.feedback_coordinate
                && v.remote_mode.is_some_and(|mode| mode.is_finite() && mode != self.handoff.local_mode) && commands.insert(&mv.node_ids.sp)
                && v.max_move.is_some_and(|n| n.is_finite() && n > self.application_tolerance && mv.max_move <= n)
                && v.hard_low.is_finite() && v.hard_high.is_finite() && v.hard_low < v.hard_high
                && mv.limits.low_low >= v.hard_low && mv.limits.high_high <= v.hard_high,
                "MV coordinate, independent feedback, mode, or hard envelope mismatch: {}", mv.name);
        }
        for (v, cv) in cvs.iter().zip(&model.variables.cvs) {
            ensure!(v.id == cv.name && v.unit == cv.units && v.pv_tag == cv.node_ids.pv
                && v.target_tag.as_deref() == Some(cv.node_ids.target.as_str())
                && v.low_tag.as_deref() == Some(cv.node_ids.limits.low.as_str())
                && v.high_tag.as_deref() == Some(cv.node_ids.limits.high.as_str())
                && cv.limits.low_low >= v.hard_low && cv.limits.high_high <= v.hard_high, "CV commissioning mismatch");
        }
        for (v, dv) in dvs.iter().zip(&model.variables.dvs) {
            ensure!(v.id == dv.name && v.unit == dv.units && v.pv_tag == dv.node_ids.pv
                && v.low_tag.as_deref() == Some(dv.node_ids.limits.low.as_str())
                && v.high_tag.as_deref() == Some(dv.node_ids.limits.high.as_str())
                && dv.limits.low >= v.hard_low && dv.limits.high <= v.hard_high, "DV commissioning mismatch");
        }
        let mut protected = HashSet::new();
        for cv in &model.variables.cvs {
            protected.extend([cv.node_ids.pv.as_str(), cv.node_ids.limits.ll.as_str(), cv.node_ids.limits.hh.as_str()]);
        }
        for mv in &model.variables.mvs {
            protected.extend([mv.node_ids.pv.as_str(), mv.node_ids.op.as_str(), mv.node_ids.mode.as_str(),
                mv.node_ids.applied_sp.as_deref().expect("validated applied feedback"),
                mv.node_ids.limits.ll.as_str(), mv.node_ids.limits.hh.as_str()]);
        }
        for dv in &model.variables.dvs { protected.insert(dv.node_ids.pv.as_str()); }
        ensure!(!protected.contains(self.acquisition_sequence_node.as_str())
            && !protected.contains(self.handoff.acknowledgement_tag.as_str())
            && self.acquisition_sequence_node != self.handoff.acknowledgement_tag,
            "Acquisition and independent DCS acknowledgement must not alias model signals");
        protected.insert(self.acquisition_sequence_node.as_str());
        protected.insert(self.handoff.acknowledgement_tag.as_str());
        let mut writable = HashSet::new();
        for tag in std::iter::once(self.handoff.release_tag.as_str()).chain(std::iter::once(self.handoff.watchdog_tag.as_str()))
            .chain(self.variables.iter().flat_map(|v| [&v.command_tag, &v.mode_target_tag, &v.target_tag, &v.low_tag, &v.high_tag]
                .into_iter().flatten().map(String::as_str))) {
            ensure!(!tag.is_empty() && !protected.contains(tag) && writable.insert(tag),
                "Commissioned writable tag aliases feedback, a hard limit, or another writer: {tag}");
        }
        for tag in model.variables.cvs.iter().flat_map(|cv| [format!("{}:Weight", cv.name), format!("{}:Alpha", cv.name)])
            .chain(model.variables.mvs.iter().map(|mv| format!("{}:Weight", mv.name))) {
            ensure!(!protected.contains(tag.as_str()) && !writable.contains(tag.as_str()), "Immutable tuning tag aliases a physical command or observation");
        }
        Ok(())
    }
    pub fn remote_modes(&self) -> Vec<f64> {
        self.variables.iter().filter(|v| v.role == "MV").map(|v| v.remote_mode.expect("validated mode")).collect()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Start, Arm, Engage, Hold, Exit, Stop,
    SwitchVersion { model_file: String, approved_sha256: String, approval_id: String,
        approval_expires_at: String, max_first_move: Vec<f64> },
    CvTarget { variable_id: String, value: f64 },
    OperatingLow { variable_id: String, value: f64 },
    OperatingHigh { variable_id: String, value: f64 },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub operation_id: String, pub profile_hash: String, pub expected_version: String,
    pub expected_epoch: u64, pub expected_instance: String, pub expires_at: String,
    pub reason: String, pub action: Action,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Message { Status, Operation { operation_id: String }, Command { request: Request } }
#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub instance_id: String, pub epoch: u64, pub model_sha256: String, pub profile_hash: String,
    pub mode: String, pub armed: bool, pub healthy: bool, pub history_samples: usize,
    pub required_history_samples: usize, pub pending_application: bool, pub dcs_handoff: bool,
    pub observed_at: Option<String>, pub operation_id: Option<String>, pub detail: String,
    pub acquired_at: Option<String>, pub acquisition_sequence: Option<u64>,
    pub sample_time: f64, pub prediction_horizon: usize, pub control_horizon: usize,
    pub maximum_solve_age_ms: u64, pub solve_acquisition_sequence: Option<u64>,
    pub forecast_sequence: Option<u64>, pub forecast_issued_at: Option<chrono::DateTime<chrono::Utc>>,
    pub writer_owner: bool, pub safe_baseline: bool, pub applied_sp: Vec<f64>,
}
#[derive(Clone, Serialize)]
pub struct Reply { pub ok: bool, pub confirmed: bool, pub detail: String, pub status: Status }
pub struct Envelope { pub request: Request, pub reply: oneshot::Sender<Reply> }
struct State {
    status: Status, last_good: Option<Instant>, seen: HashSet<String>, results: HashMap<String, Reply>,
    journal: Option<std::fs::File>, maximum_age: Duration, acquired: Option<Instant>, desired_mode: f64,
    writer_locks: Vec<std::fs::File>, expected_writers: usize,
}
impl State {
    fn fresh(&self) -> bool {
        let solve_age = Duration::from_millis(self.status.maximum_solve_age_ms);
        self.acquired.is_some_and(|at| at.elapsed() <= self.maximum_age)
            && self.last_good.is_some_and(|at| at.elapsed() <= solve_age)
    }
}
#[derive(Clone)]
pub struct Native { state: Arc<Mutex<State>> }
impl Native {
    pub fn new(instance: &str, digest: &str, profile: Option<&Commissioning>) -> Self {
        Self { state: Arc::new(Mutex::new(State { status: Status {
            instance_id: instance.into(), epoch: 1, model_sha256: digest.into(),
            profile_hash: profile.map_or_else(String::new, |p| p.profile_hash.clone()),
            mode: "monitor".into(), armed: false, healthy: false, history_samples: 0,
            required_history_samples: 0, pending_application: false, dcs_handoff: false,
            observed_at: None, operation_id: None, detail: "Waiting for coherent acquired history".into(),
            sample_time: 0.0, prediction_horizon: 0, control_horizon: 0,
            maximum_solve_age_ms: profile.map_or(1, |p| p.maximum_age_ms), solve_acquisition_sequence: None,
            forecast_sequence: None, forecast_issued_at: None,
            acquired_at: None, acquisition_sequence: None,
            writer_owner: false, safe_baseline: false,
            applied_sp: Vec::with_capacity(profile.map_or(0, |p| p.variables.iter().filter(|v| v.role == "MV").count())),
        }, last_good: None, seen: HashSet::new(), results: HashMap::new(), journal: None, acquired: None,
            writer_locks: Vec::new(), expected_writers: profile.map_or(0, |p| p.variables.iter().filter(|v| v.role == "MV").count()),
            maximum_age: Duration::from_millis(profile.map_or(1, |p| p.maximum_age_ms)), desired_mode: 1.0 })) }
    }
    pub fn own_writers(&self, locks: Vec<std::fs::File>) -> Result<()> {
        let mut state = self.state.lock().expect("native state");
        ensure!(state.writer_locks.is_empty() && locks.len() == state.expected_writers, "Incomplete or replaceable native writer ownership");
        state.status.writer_owner = !locks.is_empty();
        state.writer_locks = locks;
        Ok(())
    }
    pub fn status(&self) -> Status {
        let state = self.state.lock().expect("native state");
        let mut status = state.status.clone();
        status.healthy &= state.fresh();
        status.safe_baseline &= status.writer_owner && !status.armed && !status.pending_application
            && status.dcs_handoff && state.acquired.is_some_and(|at| at.elapsed() <= state.maximum_age);
        status
    }
    pub fn reply(&self, ok: bool, confirmed: bool, detail: impl Into<String>) -> Reply {
        Reply { ok, confirmed, detail: detail.into(), status: self.status() }
    }
    pub fn finish(&self, envelope: Envelope, ok: bool, confirmed: bool, detail: impl Into<String>) {
        let mut reply = self.reply(ok, confirmed, detail);
        reply.status.operation_id = Some(envelope.request.operation_id.clone());
        let mut state = self.state.lock().expect("native state");
        if !state.results.contains_key(&envelope.request.operation_id) {
            let persisted = (|| -> Result<()> {
                if let Some(file) = state.journal.as_mut() {
                    serde_json::to_writer(&mut *file, &reply)?; file.write_all(b"\n")?; file.sync_all()?;
                }
                Ok(())
            })();
            if persisted.is_err() {
                reply.ok = false; reply.confirmed = false;
                reply.detail = "Native result persistence failed; reconcile physical outcome".into();
                state.status.armed = false; state.status.mode = "hold".into(); state.desired_mode = 1.0;
                state.status.epoch += 1; state.status.healthy = false;
                state.status.dcs_handoff = false; state.status.safe_baseline = false;
                state.status.detail = reply.detail.clone();
                reply.status = state.status.clone();
                reply.status.operation_id = Some(envelope.request.operation_id.clone());
            }
            state.results.insert(envelope.request.operation_id.clone(), reply.clone());
        }
        let _ = envelope.reply.send(reply);
    }
    fn operation(&self, operation_id: &str) -> Reply {
        let result = self.state.lock().expect("native state").results.get(operation_id).cloned();
        result.unwrap_or_else(|| self.reply(false, false, "Unknown or unresolved operation; never replay"))
    }
    pub fn desired_mode(&self) -> f64 { self.state.lock().expect("native state").desired_mode }
    pub fn renews_watchdog(&self) -> bool {
        matches!(self.state.lock().expect("native state").status.mode.as_str(), "monitor" | "armed" | "engaged")
    }
    pub fn replayed(&self, operation_id: &str) -> bool {
        self.state.lock().expect("native state").seen.contains(operation_id)
    }
    pub fn model_metadata(&self, model: &UnifiedModel) {
        let mut state = self.state.lock().expect("native state");
        state.status.sample_time = model.tuning.sample_time;
        state.status.prediction_horizon = model.tuning.prediction_horizon;
        state.status.control_horizon = model.tuning.control_horizon;
        state.status.maximum_solve_age_ms = (model.tuning.sample_time * 1250.0).floor() as u64;
    }
    pub fn validate(&self, request: &Request) -> Result<()> {
        let mut state = self.state.lock().expect("native state");
        ensure!(!request.operation_id.is_empty() && request.operation_id.len() <= 128 && !request.reason.trim().is_empty(), "Operation identity/reason required");
        ensure!(!state.seen.contains(&request.operation_id), "Operation replay rejected; reconcile its original result");
        if state.seen.len() >= 100_000 {
            if state.status.mode != "hold" { state.status.epoch += 1; }
            state.status.armed = false; state.status.healthy = false; state.status.mode = "hold".into();
            state.status.dcs_handoff = false; state.status.safe_baseline = false; state.desired_mode = 1.0;
            state.status.detail = "Operation journal capacity exhausted; controlled restart required".into();
            anyhow::bail!("{}", state.status.detail);
        }
        // Reserve even rejected identities, so a stale request can never become valid later.
        state.seen.insert(request.operation_id.clone());
        if let Some(file) = state.journal.as_mut() {
            serde_json::to_writer(&mut *file, request)?; file.write_all(b"\n")?; file.sync_all()?;
        }
        ensure!(!state.status.profile_hash.is_empty() && request.profile_hash == state.status.profile_hash
            && request.expected_version == state.status.model_sha256 && request.expected_instance == state.status.instance_id
            && request.expected_epoch == state.status.epoch, "Stale instance, epoch, model or profile");
        let expires = chrono::DateTime::parse_from_rfc3339(&request.expires_at)?;
        let remaining = expires.signed_duration_since(chrono::Utc::now()).num_milliseconds();
        ensure!(remaining > 0 && remaining <= 300_000, "Expired or unbounded operation");
        if matches!(request.action, Action::Arm | Action::Engage | Action::SwitchVersion { .. }
            | Action::CvTarget { .. } | Action::OperatingLow { .. } | Action::OperatingHigh { .. }) {
            ensure!(state.status.healthy && state.fresh()
                && !state.status.pending_application, "No fresh healthy acquisition/history or previous command unresolved");
        }
        match request.action {
            Action::Arm => ensure!(state.status.mode == "monitor", "Explicit Monitor is required before arming"),
            Action::Engage => ensure!(state.status.armed && state.status.mode == "armed", "Explicit arm required"),
            Action::SwitchVersion { .. } => ensure!(state.status.armed && state.status.mode == "engaged", "Handoff requires already armed healthy engaged epoch"),
            _ => {}
        }
        state.status.operation_id = Some(request.operation_id.clone());
        Ok(())
    }
    pub fn monitor(&self) {
        let mut state = self.state.lock().expect("native state");
        state.status.epoch += 1; state.status.armed = false; state.status.mode = "monitor".into();
        state.status.dcs_handoff = false; state.desired_mode = 1.0;
    }
    pub fn arm(&self) {
        let mut state = self.state.lock().expect("native state");
        state.status.epoch += 1; state.status.armed = true; state.status.mode = "armed".into();
    }
    pub fn engage(&self) { self.state.lock().expect("native state").desired_mode = 2.0; }
    /// Explicit recovery invalidates old intent even if authority was already held.
    pub fn invalidate_epoch(&self) { self.state.lock().expect("native state").status.epoch += 1; }
    pub fn fault(&self, detail: impl Into<String>) {
        let mut state = self.state.lock().expect("native state");
        if state.status.mode != "hold" { state.status.epoch += 1; }
        state.status.armed = false; state.status.healthy = false; state.status.mode = "hold".into();
        state.status.dcs_handoff = false;
        state.status.safe_baseline = false;
        state.status.forecast_sequence = None;
        state.status.forecast_issued_at = None;
        state.status.detail = detail.into(); state.last_good = None; state.desired_mode = 1.0;
    }
    pub fn observe(&self, ready: bool, samples: (usize, usize), pending: bool,
        solved_at: Instant, issued_at: chrono::DateTime<chrono::Utc>, sequence: f64) {
        let mut state = self.state.lock().expect("native state");
        state.last_good = Some(solved_at);
        state.status.observed_at = Some(issued_at.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true));
        state.status.solve_acquisition_sequence = Some(sequence as u64);
        state.status.forecast_sequence = None;
        state.status.forecast_issued_at = None;
        state.status.healthy = ready; state.status.history_samples = samples.0;
        state.status.required_history_samples = samples.1; state.status.pending_application = pending;
        // A ready solve clears warmup, but must not erase a latched Hold reason.
        if ready && state.status.mode != "hold" {
            state.status.detail.clear();
        }
    }
    pub fn forecast_sequence(&self, sequence: u64, issued_at: chrono::DateTime<chrono::Utc>) {
        let mut state = self.state.lock().expect("native state");
        state.status.forecast_sequence = Some(sequence);
        state.status.forecast_issued_at = Some(issued_at);
    }
    pub fn acquired(&self, sequence: f64, acquired_after: Instant, applied_sp: &[f64], dcs_released: bool) {
        let mut state = self.state.lock().expect("native state");
        state.acquired = Some(acquired_after);
        state.status.acquired_at = Some((chrono::Utc::now() - chrono::Duration::milliseconds(acquired_after.elapsed().as_millis().min(i64::MAX as u128) as i64)).to_rfc3339());
        state.status.acquisition_sequence = Some(sequence as u64);
        state.status.applied_sp.clear();
        state.status.applied_sp.extend_from_slice(applied_sp);
        state.status.dcs_handoff = dcs_released;
        state.status.safe_baseline = dcs_released && applied_sp.len() == state.expected_writers
            && !applied_sp.is_empty() && applied_sp.iter().all(|value| value.is_finite());
    }
    pub fn dispatched(&self) { self.state.lock().expect("native state").status.pending_application = true; }
    pub fn applied(&self, remote: bool) {
        let mut state = self.state.lock().expect("native state");
        state.status.pending_application = false;
        if remote && state.status.armed && state.desired_mode == 2.0 { state.status.mode = "engaged".into(); }
    }
    pub fn cutover(&self, digest: &str) {
        let mut state = self.state.lock().expect("native state");
        state.status.epoch += 1; state.status.model_sha256 = digest.into(); state.status.pending_application = true;
        state.status.healthy = false; state.last_good = None;
        state.status.observed_at = None; state.status.solve_acquisition_sequence = None;
        state.status.forecast_sequence = None;
        state.status.forecast_issued_at = None;
    }
    pub fn released(&self, stop: bool) {
        let mut state = self.state.lock().expect("native state");
        state.status.dcs_handoff = true; state.status.pending_application = false;
        if stop { state.status.mode = "stopped".into(); }
    }
}

/// Socket directory and socket are private to the supervisor service account. Never
/// unlink an existing socket: bind failure protects against a second active writer.
pub fn listen(path: &Path, native: Native) -> Result<mpsc::Receiver<Envelope>> {
    ensure!(native.status().writer_owner, "Native IPC requires held OS writer ownership");
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    ensure!(path.is_absolute(), "Authority socket must be absolute");
    let parent = path.parent().context("Authority socket needs parent")?;
    let metadata = std::fs::metadata(parent)?;
    ensure!(metadata.is_dir() && metadata.mode() & 0o077 == 0, "Authority socket directory must be private (0700)");
    let listener = UnixListener::bind(path).context("Authority socket already exists or cannot bind; no second writer permitted")?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    native.state.lock().expect("native state").journal = Some(std::fs::OpenOptions::new()
        .write(true).create_new(true).open(parent.join("operations.ndjson"))?);
    let (tx, rx) = mpsc::channel::<Envelope>(16);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { break; };
            let native = native.clone(); let tx = tx.clone();
            tokio::spawn(async move {
                let (reader, mut writer) = stream.into_split();
                let mut reader = BufReader::new(reader.take(65_537));
                let mut line = String::new();
                let reply = match tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line)).await {
                    Ok(Ok(n)) if n > 0 && n <= 65_536 && line.ends_with('\n') => {
                        match serde_json::from_str::<Message>(&line) {
                            Ok(Message::Status) => native.reply(true, true, "Native status; freshness is local acquisition evidence"),
                            Ok(Message::Operation { operation_id }) => native.operation(&operation_id),
                            Ok(Message::Command { request }) => {
                                let (reply, receive) = oneshot::channel();
                                if tx.try_send(Envelope { request, reply }).is_err() { native.reply(false, false, "Native command queue busy") }
                                else { match tokio::time::timeout(Duration::from_secs(305), receive).await {
                                    Ok(Ok(reply)) => reply,
                                    _ => native.reply(false, false, "Native outcome unresolved; reconcile status, never replay"),
                                } }
                            }
                            Err(error) => native.reply(false, false, format!("Invalid native request: {error}")),
                        }
                    }
                    _ => native.reply(false, false, "Missing, oversized or timed-out JSONL frame"),
                };
                if let Ok(mut bytes) = serde_json::to_vec(&reply) {
                    bytes.push(b'\n');
                    let _ = tokio::time::timeout(Duration::from_secs(2), writer.write_all(&bytes)).await;
                }
            });
        }
    });
    Ok(rx)
}

/// OS locks survive supervisor death as long as its native child is still alive.
pub fn lock_writers(socket: &Path, profile: &Commissioning) -> Result<Vec<std::fs::File>> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use sha2::{Digest, Sha256};
    let root = socket.parent().and_then(Path::parent).context("Missing authority lock domain")?;
    ensure!(root.is_absolute() && std::fs::metadata(root)?.mode() & 0o077 == 0, "Authority lock domain must be private");
    let mut locks = Vec::new();
    for v in profile.variables.iter().filter(|v| v.role == "MV") {
        let key = format!("{}\0{}", profile.endpoint, v.command_tag.as_deref().context("Missing command coordinate")?);
        let path = root.join(format!("writer-{:x}.lock", Sha256::digest(key.as_bytes())));
        let file = std::fs::OpenOptions::new().read(true).write(true).create(true)
            .mode(0o600).custom_flags(libc::O_NOFOLLOW).open(path)?;
        ensure!(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "Another native process owns commissioned MV {}", v.id);
        locks.push(file);
    }
    Ok(locks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_observation_clears_warmup_but_preserves_latched_fault() {
        let native = Native::new("test", "digest", None);
        {
            let mut state = native.state.lock().unwrap();
            state.maximum_age = Duration::from_secs(60);
            state.status.maximum_solve_age_ms = 60_000;
        }
        native.acquired(2.0, Instant::now(), &[], true);
        native.observe(false, (48, 49), false, Instant::now(), chrono::Utc::now(), 2.0);
        assert!(!native.status().healthy);
        assert!(!native.status().detail.is_empty());
        native.observe(true, (49, 49), false, Instant::now(), chrono::Utc::now(), 2.0);
        assert!(native.status().healthy);
        assert!(native.status().detail.is_empty());

        native.fault("independent handoff failure");
        native.observe(true, (50, 49), false, Instant::now(), chrono::Utc::now(), 2.0);
        assert_eq!(native.status().mode, "hold");
        assert_eq!(native.status().detail, "independent handoff failure");
        native.monitor();
        native.observe(true, (51, 49), false, Instant::now(), chrono::Utc::now(), 2.0);
        assert!(native.status().detail.is_empty());
    }

    #[test]
    fn forecast_binding_is_invalidated_by_new_solve_fault_and_cutover() {
        let native = Native::new("test", "digest", None);
        let first = chrono::DateTime::from_timestamp(1_767_225_600, 123_456_789).unwrap();
        native.observe(true, (49, 49), false, Instant::now(), first, 2.0);
        native.forecast_sequence(10, first);
        let qualified = serde_json::to_value(native.status()).unwrap();
        assert_eq!(qualified["forecast_sequence"], 10);
        assert_eq!(qualified["forecast_issued_at"], first.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true));
        assert_eq!(qualified["forecast_issued_at"], qualified["observed_at"]);

        let next = first + chrono::Duration::seconds(60);
        native.observe(true, (50, 49), false, Instant::now(), next, 4.0);
        let awaiting_forecast = native.status();
        assert_eq!(awaiting_forecast.forecast_sequence, None);
        assert_eq!(awaiting_forecast.forecast_issued_at, None);

        // A delayed prior forecast must not borrow the newer solve's timestamp.
        native.forecast_sequence(11, first);
        let delayed = serde_json::to_value(native.status()).unwrap();
        assert_ne!(delayed["forecast_issued_at"], delayed["observed_at"]);
        native.forecast_sequence(12, next);
        let current = serde_json::to_value(native.status()).unwrap();
        assert_eq!(current["forecast_issued_at"], current["observed_at"]);

        native.fault("acquisition lost");
        assert_eq!(native.status().forecast_sequence, None);
        assert_eq!(native.status().forecast_issued_at, None);
        native.observe(true, (51, 49), false, Instant::now(), next, 6.0);
        native.forecast_sequence(13, next);
        native.cutover("replacement");
        let replaced = native.status();
        assert_eq!(replaced.observed_at, None);
        assert_eq!(replaced.forecast_sequence, None);
        assert_eq!(replaced.forecast_issued_at, None);
    }
}
