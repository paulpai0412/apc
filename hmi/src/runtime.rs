//! Commissioned authority and durable intent. This module never infers a field mode contract.
use std::{collections::{BTreeMap, BTreeSet}, str::FromStr, sync::Arc};
use axum::{extract::{Path, State}, http::StatusCode, response::{IntoResponse, Response}, routing::{get, post}, Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous}, Row, SqlitePool};
use tokio::sync::{broadcast, Mutex, RwLock};
use crate::{auth::{CsrfVerified, CurrentUser}, opc_worker::OpcUpdate, state::AppState};
#[path = "runtime_performance.rs"]
mod performance;
#[path = "runtime_transport.rs"]
mod transport;
#[path = "runtime_publisher.rs"]
pub(crate) mod publisher;
#[path = "runtime_safety.rs"]
pub(crate) mod safety;
#[path = "runtime_history.rs"]
mod history;
#[path = "runtime_maintenance.rs"]
mod maintenance_overview;

type Result<T, E = RuntimeError> = std::result::Result<T, E>;
#[derive(Debug)]
pub struct RuntimeError(pub StatusCode, pub String);
impl IntoResponse for RuntimeError {
    fn into_response(self) -> Response { (self.0, Json(json!({"error":self.1}))).into_response() }
}
fn invalid(message: impl Into<String>) -> RuntimeError { RuntimeError(StatusCode::CONFLICT, message.into()) }
fn storage(error: impl std::fmt::Display) -> RuntimeError {
    tracing::error!("Runtime registry: {error}");
    RuntimeError(StatusCode::SERVICE_UNAVAILABLE, "Runtime registry unavailable; no new operation is permitted".into())
}
fn digest(value: &[u8]) -> String { format!("{:x}", Sha256::digest(value)) }

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Target { pub plant: String, pub line: String, pub area: String, pub group: String }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant { pub user_id: i64, pub permissions: BTreeSet<String> }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Variable {
    pub id: String, pub equipment_instance_id: String, pub role: String, pub unit: String,
    pub pv_tag: String, pub command_tag: Option<String>, pub applied_tag: Option<String>,
    pub op_tag: Option<String>, pub mode_tag: Option<String>, pub mode_target_tag: Option<String>,
    pub target_tag: Option<String>, pub low_tag: Option<String>, pub high_tag: Option<String>,
    pub hard_low: f64, pub hard_high: f64, pub max_move: Option<f64>,
    pub command_coordinate: Option<String>, pub feedback_coordinate: Option<String>,
    pub remote_mode: Option<f64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffContract {
    pub release_tag: String, pub release_value: f64, pub acknowledgement_tag: String,
    pub acknowledgement_value: f64, pub timeout_ms: u64, pub watchdog_evidence: String,
    pub acquire_value:f64,
    pub watchdog_tag:String,
    pub watchdog_timeout_ms:u64,
    pub local_mode:f64,
}
/// Administrator-provisioned, independently commissioned file; not a candidate payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentProfile {
    pub profile_hash: String, pub target: Target, pub equipment_instance_ids: BTreeSet<String>,
    pub engine_id: String, pub model_sha256: String, pub endpoint: String,
    pub model_file: String,
    pub model_binding: Value,
    pub equipment_view: Option<Value>,
    pub forecast_tag: String,
    pub observation_tag: String,
    pub variables: Vec<Variable>, pub grants: Vec<Grant>,
    pub approved_by: String, pub commissioning_evidence: String,
    pub commissioning_kind:String,
    pub operating_envelope: String, pub maximum_age_ms: u64, pub maximum_skew_ms: u64,
    pub application_tolerance:f64,
    pub handoff: HandoffContract,
    pub performance: Option<performance::PerformanceContract>,
    pub acquisition_sequence_node:String,
    pub history_capacity_samples:usize,
}
impl DeploymentProfile {
    /// Hash the canonical serde JSON with an empty profile_hash field. Includes all authority and grants.
    pub fn calculated_hash(&self) -> std::result::Result<String, String> {
        let mut copy = self.clone(); copy.profile_hash.clear();
        serde_json::to_vec(&copy).map(|bytes| digest(&bytes)).map_err(|e| e.to_string())
    }
    fn permits(&self, user: &CurrentUser, permission: &str) -> bool {
        let role_allowed=match permission {
            "runtime.view"=>true,
            "runtime.operate"=>matches!(user.role,crate::auth::Role::Operator|crate::auth::Role::Engineer|crate::auth::Role::Admin),
            "runtime.deploy"|"runtime.approve"|"runtime.publish"|"runtime.publish.grant"=>
                matches!(user.role,crate::auth::Role::Engineer|crate::auth::Role::Admin),
            "runtime.admin"|"runtime.maintenance"=>user.role==crate::auth::Role::Admin,
            _=>false,
        };
        role_allowed&&self.grants.iter().any(|grant| grant.user_id == user.id && grant.permissions.contains(permission))
    }
    fn validate(&self) -> std::result::Result<(), String> {
        if self.calculated_hash()? != self.profile_hash || self.approved_by.trim().is_empty()
            || self.commissioning_evidence.trim().is_empty() || self.operating_envelope.trim().is_empty()
            || self.equipment_instance_ids.is_empty() || self.maximum_age_ms == 0 || self.maximum_age_ms > 3_600_000
            || self.maximum_skew_ms > self.maximum_age_ms || self.variables.is_empty()
            || !self.application_tolerance.is_finite() || self.application_tolerance<0.0
            || self.model_sha256.len() != 64 || self.engine_id.is_empty()
            || [&self.target.plant,&self.target.line,&self.target.area,&self.target.group].iter().any(|s| s.is_empty()) {
            return Err("Profile identity, scope, commissioning evidence or freshness contract is invalid".into());
        }
        let binding: crate::model_binding::ModelBinding = serde_json::from_value(self.model_binding.clone())
            .map_err(|_| "Profile requires a valid explicit model binding")?;
        if binding.controller_name != self.engine_id || self.model_file.is_empty()
            || self.forecast_tag.is_empty() || self.observation_tag.is_empty() {
            return Err("Profile model and native protocol identity are incomplete".into());
        }
        if !matches!(self.commissioning_kind.as_str(),"synthetic_local"|"field") {
            return Err("Commissioning kind must explicitly distinguish synthetic_local from field".into());
        }
        let endpoint=reqwest::Url::parse(&self.endpoint).map_err(|_|"Invalid commissioned OPC endpoint")?;
        if endpoint.scheme()!="opc.tcp"||!endpoint.username().is_empty()||endpoint.password().is_some()
            ||endpoint.query().is_some()||endpoint.fragment().is_some() {
            return Err("Commissioned OPC endpoint must be explicit OPC UA without embedded credentials".into());
        }
        if self.commissioning_kind=="synthetic_local" {
            let local=endpoint.host_str().and_then(|host|host.trim_matches(['[',']']).parse::<std::net::IpAddr>().ok()).is_some_and(|address|address.is_loopback());
            if !local{return Err("Synthetic-local commissioning may only use an explicit loopback IP endpoint".into());}
        }
        if self.acquisition_sequence_node.is_empty() || !(2..=8192).contains(&self.history_capacity_samples) {
            return Err("Explicit acquisition sequence and bounded history capacity required".into());
        }
        if let Some(contract)=&self.performance {
            if contract.regime_tags.len()<4 || contract.regime_tags.len()>32
                || contract.regime_tags.iter().any(|tag|tag.is_empty()||tag.len()>512)
                || contract.sample_interval_ms==0 || contract.minimum_samples<2 {
                return Err("Performance requires product/load/material/mode regime tags and a valid sampling contract".into());
            }
        }
        if binding.cvs.len()+binding.mvs.len()+binding.dvs.len()!=self.variables.len() {
            return Err("Profile variables and explicit model binding dimensions differ".into());
        }
        for variable in &self.variables {
            let key=match variable.role.as_str(){"CV"=>"cvs","MV"=>"mvs","DV"=>"dvs",_=>return Err("Unknown profile role".into())};
            let bound=self.model_binding[key].as_array().and_then(|variables|variables.iter().find(|v|v["name"]==variable.id))
                .ok_or("Profile variable ID must exactly match its model-binding name")?;
            let nodes=&bound["node_ids"];
            if bound["units"]!=variable.unit || nodes["pv"]!=variable.pv_tag
                || nodes["limits"]["low"].as_str()!=variable.low_tag.as_deref()
                || nodes["limits"]["high"].as_str()!=variable.high_tag.as_deref()
                || (variable.role=="CV"&&nodes["target"].as_str()!=variable.target_tag.as_deref())
                || (variable.role=="MV"&&(nodes["sp"].as_str()!=variable.command_tag.as_deref()
                    || nodes["applied_sp"].as_str()!=variable.applied_tag.as_deref()
                    || nodes["op"].as_str()!=variable.op_tag.as_deref()
                    || nodes["mode"].as_str()!=variable.mode_tag.as_deref()
                    || nodes["mode_target"].as_str()!=variable.mode_target_tag.as_deref())) {
                return Err("Profile variable mapping differs from the explicit model binding".into());
            }
        }
        let url = reqwest::Url::parse(&self.endpoint).map_err(|_| "Invalid commissioned endpoint")?;
        if url.scheme() != "opc.tcp" || url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
            return Err("Profile requires a credential-free exact OPC UA endpoint".into());
        }
        let h = &self.handoff;
        if h.release_tag.is_empty() || h.acknowledgement_tag.is_empty() || h.release_tag == h.acknowledgement_tag
            || !h.release_value.is_finite() || !h.acquire_value.is_finite() || h.acquire_value==h.release_value
            || !h.local_mode.is_finite() || !h.acknowledgement_value.is_finite() || h.timeout_ms == 0 || h.timeout_ms>300_000
            || h.watchdog_tag.is_empty() || h.watchdog_tag==h.release_tag || h.watchdog_tag==h.acknowledgement_tag
            || h.watchdog_timeout_ms==0 || h.watchdog_timeout_ms>3_600_000
            || h.watchdog_evidence.trim().is_empty() { return Err("Explicit independent DCS acquisition/release/watchdog contract required".into()); }
        let mut ids = BTreeSet::new(); let mut writes = BTreeSet::new();
        for v in &self.variables {
            if !ids.insert(&v.id) || v.id.is_empty() || v.unit.is_empty() || v.pv_tag.is_empty()
                || !self.equipment_instance_ids.contains(&v.equipment_instance_id)
                || ![v.hard_low,v.hard_high].iter().all(|v| v.is_finite()) || v.hard_low >= v.hard_high
                || !matches!(v.role.as_str(), "CV"|"MV"|"DV") { return Err("Invalid variable identity, role, units or engineering bounds".into()); }
            if v.role == "MV" && (v.command_tag.as_deref().is_none_or(str::is_empty)
                || v.applied_tag.as_deref().is_none_or(str::is_empty) || v.command_tag == v.applied_tag
                || v.mode_tag.as_deref().is_none_or(str::is_empty) || v.mode_target_tag.as_deref().is_none_or(str::is_empty)
                || !v.max_move.is_some_and(|n| n.is_finite() && n > 0.0)
                || !v.remote_mode.is_some_and(f64::is_finite)
                || v.command_coordinate.as_deref().is_none_or(str::is_empty)
                || v.command_coordinate != v.feedback_coordinate) {
                return Err("MV requires exact independent applied-SP, coordinate, mode and maximum-move mapping".into());
            }
            for tag in [&v.command_tag,&v.mode_target_tag,&v.target_tag,&v.low_tag,&v.high_tag].into_iter().flatten() {
                if tag.is_empty() || !writes.insert(tag) { return Err("Ambiguous writable physical tag".into()); }
            }
        }
        let read_only: BTreeSet<&str> = self.variables.iter().flat_map(|v|
            std::iter::once(v.pv_tag.as_str()).chain([v.applied_tag.as_deref(),v.op_tag.as_deref(),v.mode_tag.as_deref()].into_iter().flatten())).collect();
        if writes.iter().any(|tag|read_only.contains(tag.as_str())) {
            return Err("Writable profile tags may not alias measurement, actual-mode or independent-feedback tags".into());
        }
        let mut protocol=BTreeSet::new();
        for tag in [&self.handoff.release_tag,&self.handoff.acknowledgement_tag,&self.handoff.watchdog_tag,
            &self.acquisition_sequence_node,&self.forecast_tag,&self.observation_tag] {
            if tag.is_empty()||!protocol.insert(tag)||writes.contains(tag)||read_only.contains(tag.as_str()) {
                return Err("Handoff/acquisition/diagnostic tags must be independent of every variable role".into());
            }
        }
        if let Some(contract)=&self.performance {
            let mut sampled=BTreeSet::new();
            for (original,tag) in &contract.sampled_tags {
                if original==tag || tag.is_empty() || tag.len()>512 || !sampled.insert(tag)
                    || writes.contains(tag) || read_only.contains(tag.as_str()) || protocol.contains(tag) {
                    return Err("Performance sampled settings need distinct producer-owned observation tags".into());
                }
            }
            for variable in self.variables.iter().filter(|v|v.role!="DV") {
                for tag in [&variable.low_tag,&variable.high_tag].into_iter()
                    .chain(std::iter::once(&variable.target_tag).filter(|_|variable.role=="CV")).flatten() {
                    if !contract.sampled_tags.contains_key(tag) {return Err("Performance sampled setting map is incomplete".into());}
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag="kind", rename_all="snake_case", deny_unknown_fields)]
pub enum OperationAction {
    CvTarget { variable_id: String, value: f64 },
    OperatingLow { variable_id: String, value: f64 },
    OperatingHigh { variable_id: String, value: f64 },
    // Empty struct variants enforce deny_unknown_fields; Serde unit variants ignore extra fields.
    Start {},
    RecoverMonitor {},
    StartCandidate {candidate_hash:String,approval_id:String},
    Monitor {},
    Arm {},
    Engage {},
    Hold {},
    Exit {},
    Stop {},
    SwitchVersion { candidate_hash:String, approval_id:String },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OperationRequest {
    pub operation_id: String, pub profile_hash: String, pub expected_version: String,
    pub expected_epoch: i64, pub expires_at: DateTime<Utc>, pub reason: String,
    pub expected_instance: Option<String>,
    pub action: OperationAction,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationRecord {
    pub operation_id: String, pub profile_hash: String, pub actor_id: i64,
    pub state: String, pub request: Value, pub detail: String,
    pub created_at: String, pub updated_at: String,
    pub blocking:bool,pub clearance_id:Option<String>,pub cleared_at:Option<String>,pub clearance_evidence:Option<Value>,
}
#[derive(Clone, Serialize)]
struct Observation { node_id: String, value: Value, timestamp: String, status: u32, received_at: DateTime<Utc>, source_endpoint:String }
#[derive(Default)]
struct WorkerConnection {reported_connected:Option<bool>,generation:u64,observed_at:Option<DateTime<Utc>>,reason:Option<&'static str>}

pub struct RuntimeStore {
    pool: SqlitePool,
    profiles: BTreeMap<String, DeploymentProfile>,
    pub(crate) runtime_enabled:bool,
    subscribed: BTreeMap<String,BTreeSet<String>>,
    observations: RwLock<BTreeMap<String, Observation>>,
    worker_connection:RwLock<WorkerConnection>,
    /// Serializes validation, durable registration and dispatch for this sole HMI owner.
    dispatch: Mutex<()>,
}
impl RuntimeStore {
    pub async fn load_profiles(path: Option<&str>) -> std::result::Result<Vec<DeploymentProfile>, String> {
        let Some(path) = path else { return Ok(Vec::new()); };
        if !std::path::Path::new(path).is_absolute() { return Err("Commissioning file must be an administrator-selected absolute path".into()); }
        let metadata = tokio::fs::metadata(path).await.map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.len() > 4 * 1024 * 1024 { return Err("Invalid or oversized commissioning file".into()); }
        let bytes = tokio::fs::read(path).await.map_err(|e| e.to_string())?;
        if bytes.len() > 4 * 1024 * 1024 { return Err("Commissioning file grew beyond limit".into()); }
        let profiles: Vec<DeploymentProfile> = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        for profile in &profiles { profile.validate()?; }
        Ok(profiles)
    }
    pub async fn open(database_url: &str, profiles: Vec<DeploymentProfile>,runtime_enabled:bool) -> std::result::Result<Self,String> {
        if profiles.len()>64{return Err("Runtime profile registry exceeds 64 commissioned groups".into());}
        let mut mapped = BTreeMap::new(); let mut mv_owners = BTreeSet::new();
        for profile in profiles {
            profile.validate()?;
            for variable in profile.variables.iter().filter(|v| v.role == "MV") {
                let key = (profile.endpoint.clone(), variable.command_tag.clone().unwrap());
                if !mv_owners.insert(key) { return Err("A physical MV has multiple commissioned owners; startup refused".into()); }
            }
            if mapped.insert(profile.profile_hash.clone(),profile).is_some() { return Err("Duplicate profile".into()); }
        }
        let options = SqliteConnectOptions::from_str(database_url).map_err(|e| e.to_string())?
            .create_if_missing(true).journal_mode(SqliteJournalMode::Wal).synchronous(SqliteSynchronous::Full);
        let pool = SqlitePool::connect_with(options).await.map_err(|e| e.to_string())?;
        for statement in [
            "CREATE TABLE IF NOT EXISTS runtime_operations (id TEXT PRIMARY KEY, profile_hash TEXT NOT NULL, actor_id INTEGER NOT NULL, state TEXT NOT NULL, request TEXT NOT NULL, detail TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS runtime_epochs (profile_hash TEXT PRIMARY KEY, epoch INTEGER NOT NULL)",
            "CREATE TABLE IF NOT EXISTS runtime_events (id INTEGER PRIMARY KEY AUTOINCREMENT, profile_hash TEXT NOT NULL, source TEXT NOT NULL, severity TEXT NOT NULL, condition TEXT NOT NULL, first_seen TEXT NOT NULL, last_seen TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS runtime_event_reads (event_id INTEGER NOT NULL, user_id INTEGER NOT NULL, read_at TEXT NOT NULL, note TEXT NOT NULL, PRIMARY KEY(event_id,user_id))",
            "CREATE TABLE IF NOT EXISTS runtime_approvals (id TEXT PRIMARY KEY, profile_hash TEXT NOT NULL, candidate_hash TEXT NOT NULL, author_id INTEGER NOT NULL, approver_id INTEGER NOT NULL, expires_at TEXT NOT NULL, binding TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS runtime_approval_revocations (approval_id TEXT PRIMARY KEY, actor_id INTEGER NOT NULL, revoked_at TEXT NOT NULL, reason TEXT NOT NULL)",
        ] { sqlx::query(statement).execute(&pool).await.map_err(|e| e.to_string())?; }
        for hash in mapped.keys() {
            if runtime_enabled {
                sqlx::query("UPDATE runtime_operations SET state='unknown',detail='Runtime owner restarted across dispatch; reconcile original receipt, never resend',updated_at=? WHERE profile_hash=? AND state='executing'")
                    .bind(Utc::now().to_rfc3339()).bind(hash).execute(&pool).await.map_err(|e|e.to_string())?;
            }
            let statement=if runtime_enabled{"INSERT INTO runtime_epochs(profile_hash,epoch) VALUES (?,1) ON CONFLICT(profile_hash) DO UPDATE SET epoch=epoch+1"}
                else{"INSERT OR IGNORE INTO runtime_epochs(profile_hash,epoch) VALUES (?,1)"};
            sqlx::query(statement).bind(hash).execute(&pool).await.map_err(|e| e.to_string())?;
        }
        let mut subscribed:BTreeMap<String,BTreeSet<String>>=BTreeMap::new();
        for profile in mapped.values() {subscribed.entry(profile.endpoint.clone()).or_default().extend(Self::tags(profile));}
        let store=Self { pool, profiles:mapped, runtime_enabled, subscribed, observations:RwLock::new(BTreeMap::new()),worker_connection:RwLock::new(WorkerConnection::default()), dispatch:Mutex::new(()) };
        safety::init(&store).await.map_err(|error|error.1)?;
        publisher::init(&store).await.map_err(|error|error.1)?;
        Ok(store)
    }
    pub(crate) fn profile(&self, hash: &str, user: &CurrentUser, permission: &str) -> Result<&DeploymentProfile> {
        let profile = self.profiles.get(hash).ok_or_else(|| invalid("Commissioned profile not found"))?;
        if !profile.permits(user,permission) { return Err(RuntimeError(StatusCode::FORBIDDEN,"No explicit permission for this commissioned scope".into())); }
        Ok(profile)
    }
    pub async fn epoch(&self, hash: &str) -> Result<i64> {
        sqlx::query_scalar("SELECT epoch FROM runtime_epochs WHERE profile_hash=?").bind(hash).fetch_one(&self.pool).await.map_err(storage)
    }
    pub async fn observe(self: Arc<Self>, mut receiver: broadcast::Receiver<OpcUpdate>) {
        let mut timer=tokio::time::interval(std::time::Duration::from_secs(1));let mut closed=false;
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _=timer.tick()=>{if let Err(error)=safety::tick(&self).await {tracing::error!("Runtime condition persistence failed: {}",error.1);}},
                result=receiver.recv(),if !closed=>match result {
                    Ok(update)=>{
                        if update.internal {
                            if update.node_id=="System:PlcConnection" {
                                let connected=match update.value.as_i64(){Some(1)=>Some(true),Some(0)=>Some(false),_=>None};
                                let mut worker=self.worker_connection.write().await;
                                if connected==Some(true)&&worker.reported_connected!=Some(true){worker.generation=worker.generation.saturating_add(1);}
                                worker.reported_connected=connected;worker.observed_at=Some(Utc::now());worker.reason=None;drop(worker);
                                if connected!=Some(true){self.observations.write().await.clear();}
                            }
                            continue;
                        }
                        if !self.subscribed.get(&update.source_endpoint).is_some_and(|tags|tags.contains(&update.node_id)){continue;}
                        self.observations.write().await.insert(update.node_id.clone(),Observation{
                            node_id:update.node_id,value:update.value,timestamp:update.timestamp,status:update.status,
                            received_at:Utc::now(),source_endpoint:update.source_endpoint});
                    },
                    Err(broadcast::error::RecvError::Lagged(_))=>{
                        self.observations.write().await.clear();
                        let mut worker=self.worker_connection.write().await;worker.reported_connected=None;worker.observed_at=Some(Utc::now());worker.reason=Some("observer_lagged");
                    },
                    Err(broadcast::error::RecvError::Closed)=>{
                        self.observations.write().await.clear();closed=true;
                        let mut worker=self.worker_connection.write().await;worker.reported_connected=None;worker.observed_at=Some(Utc::now());worker.reason=Some("observer_closed");
                    },
                }
            }
        }
    }
    pub(crate) fn tags(profile: &DeploymentProfile) -> BTreeSet<String> {
        let mut tags = BTreeSet::from([profile.handoff.acknowledgement_tag.clone(),profile.handoff.release_tag.clone(),
            profile.handoff.watchdog_tag.clone(),profile.acquisition_sequence_node.clone(),
            profile.forecast_tag.clone(),profile.observation_tag.clone()]);
        for v in &profile.variables {
            tags.insert(v.pv_tag.clone());
            for tag in [&v.command_tag,&v.applied_tag,&v.op_tag,&v.mode_tag,&v.mode_target_tag,&v.target_tag,&v.low_tag,&v.high_tag].into_iter().flatten() { tags.insert(tag.clone()); }
        }
        fn node_tags(value:&Value,tags:&mut BTreeSet<String>) {
            match value {Value::String(tag)=>{tags.insert(tag.clone());},Value::Object(map)=>for value in map.values(){node_tags(value,tags);},_=>{}}
        }
        for key in ["cvs","mvs","dvs"] {
            if let Some(variables)=profile.model_binding[key].as_array() {
                for variable in variables {node_tags(&variable["node_ids"],&mut tags);}
            }
        }
        if let Some(contract)=&profile.performance {
            tags.extend(contract.regime_tags.iter().cloned());
            tags.extend(contract.sampled_tags.values().cloned());
        }
        tags
    }
    async fn operation(&self, id: &str) -> Result<Option<OperationRecord>> {
        let row=sqlx::query("SELECT o.*,c.clearance_id,s.created_at AS cleared_at,s.evidence FROM runtime_operations o LEFT JOIN runtime_operation_clearances c ON c.operation_id=o.id LEFT JOIN runtime_clearances s ON s.id=c.clearance_id WHERE o.id=?")
            .bind(id).fetch_optional(&self.pool).await.map_err(storage)?;
        row.map(|r| {
            let state:String=r.get("state");let clearance_id:Option<String>=r.get("clearance_id");
            Ok(OperationRecord {
                operation_id:r.get("id"),profile_hash:r.get("profile_hash"),actor_id:r.get("actor_id"),
                blocking:state=="executing"||(state=="unknown"&&clearance_id.is_none()),state,
                request:serde_json::from_str(r.get::<&str,_>("request")).map_err(storage)?,detail:r.get("detail"),
                created_at:r.get("created_at"),updated_at:r.get("updated_at"),clearance_id,cleared_at:r.get("cleared_at"),
                clearance_evidence:r.get::<Option<String>,_>("evidence").map(|value|serde_json::from_str(&value).map_err(storage)).transpose()?,
            })
        }).transpose()
    }
    async fn event(&self, profile: &str, source: &str, severity: &str, condition: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        sqlx::query("INSERT INTO runtime_events(profile_hash,source,severity,condition,first_seen,last_seen) VALUES (?,?,?,?,?,?)")
            .bind(profile).bind(source).bind(severity).bind(condition).bind(&now).bind(&now).execute(&self.pool).await.map_err(storage)?;
        Ok(())
    }
    async fn finish(&self, id: &str, state: &str, detail: &str) -> Result<()> {
        sqlx::query("UPDATE runtime_operations SET state=?,detail=?,updated_at=? WHERE id=? AND state='executing'")
            .bind(state).bind(detail).bind(Utc::now().to_rfc3339()).bind(id).execute(&self.pool).await.map_err(storage)?;
        Ok(())
    }
    async fn reconcile_result(&self,id:&str,state:&str,detail:&str)->Result<()> {
        if !matches!(state,"accepted"|"rejected"){return Err(invalid("Invalid original operation receipt state"));}
        sqlx::query("UPDATE runtime_operations SET state=?,detail=?,updated_at=? WHERE id=? AND state='unknown' AND NOT EXISTS (SELECT 1 FROM runtime_operation_clearances c WHERE c.operation_id=runtime_operations.id)")
            .bind(state).bind(detail).bind(Utc::now().to_rfc3339()).bind(id).execute(&self.pool).await.map_err(storage)?;
        Ok(())
    }
    async fn pending_count(&self,profile:&str)->Result<i64> {
        sqlx::query_scalar("SELECT COUNT(*) FROM runtime_operations o LEFT JOIN runtime_operation_clearances c ON c.operation_id=o.id WHERE o.profile_hash=? AND (o.state='executing' OR (o.state='unknown' AND c.operation_id IS NULL))")
            .bind(profile).fetch_one(&self.pool).await.map_err(storage)
    }
    pub(crate) async fn installation_intent(
        &self, user:&CurrentUser, profile_hash:&str, approval_id:&str, operation_id:&str,
        epoch:i64, expires_at:DateTime<Utc>, request:Value,
        bundle:&crate::control_versions::FrozenVersionBundle,
    ) -> Result<(OperationRecord,bool)> {
        let _guard=self.dispatch.lock().await;
        let profile=self.profile(profile_hash,user,"runtime.deploy")?;
        if let Some(existing)=self.operation(operation_id).await? {
            if existing.actor_id!=user.id || existing.request!=request {return Err(invalid("Operation ID binds a different installation intent"));}
            return Ok((existing,false));
        }
        if operation_id.is_empty() || operation_id.len()>128 || !operation_id.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-'||b==b'_')
            || expires_at<=Utc::now() || expires_at.signed_duration_since(Utc::now()).num_seconds()>60
            || epoch!=self.epoch(profile_hash).await? {return Err(invalid("Installation ID, expiry or epoch is invalid"));}
        let binding=self.approved_binding(profile,&bundle.version_id,approval_id).await?;
        if binding["model_sha256"]!=bundle.model_sha256 || binding["manifest_sha256"]!=bundle.manifest_sha256 {
            return Err(invalid("Approved candidate bytes changed"));
        }
        verify_candidate_model(profile,&bundle.model_json)?;
        let pending=self.pending_count(profile_hash).await?;
        if pending>0{return Err(invalid("Unresolved target operation blocks installation"));}
        let now=Utc::now().to_rfc3339();
        sqlx::query("INSERT INTO runtime_operations VALUES (?,?,?,'executing',?,'Installation intent committed; no start requested',?,?)")
            .bind(operation_id).bind(profile_hash).bind(user.id).bind(request.to_string()).bind(&now).bind(&now)
            .execute(&self.pool).await.map_err(storage)?;
        Ok((self.operation(operation_id).await?.ok_or_else(||storage("Lost installation intent"))?,true))
    }
    pub(crate) async fn installation_result(&self,id:&str,state:&str,detail:&str)->Result<()> {
        if !matches!(state,"accepted"|"rejected"|"unknown"){return Err(invalid("Invalid installation outcome"));}
        self.finish(id,state,detail).await
    }
    pub(crate) async fn reconcile_intent(&self,user:&CurrentUser,id:&str,profile_hash:&str,permission:&str,request:Value,expires_at:DateTime<Utc>)->Result<OperationRecord> {
        let _guard=self.dispatch.lock().await;
        self.profile(profile_hash,user,permission)?;
        if id.is_empty()||id.len()>128||!id.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-'||b==b'_') {
            return Err(invalid("Invalid original operation ID"));
        }
        if let Some(record)=self.operation(id).await? {
            if record.actor_id!=user.id||record.request!=request{return Err(invalid("Operation ID already binds a different intent"));}
            return Ok(record);
        }
        if expires_at>=Utc::now(){return Err(invalid("Wait for original intent expiry before proving it was never registered"));}
        let now=Utc::now().to_rfc3339();
        sqlx::query("INSERT INTO runtime_operations VALUES (?,?,?,'rejected',?,'Expired intent was not registered; durable tombstone prevents late dispatch',?,?)")
            .bind(id).bind(profile_hash).bind(user.id).bind(request.to_string()).bind(&now).bind(&now)
            .execute(&self.pool).await.map_err(storage)?;
        self.event(profile_hash,id,"info","Expired unregistered intent reconciled without dispatch").await?;
        self.operation(id).await?.ok_or_else(||storage("Lost reconciliation tombstone"))
    }
    pub(crate) async fn approved_binding(&self,profile:&DeploymentProfile,candidate:&str,id:&str)->Result<Value> {
        let row=sqlx::query("SELECT a.* FROM runtime_approvals a LEFT JOIN runtime_approval_revocations r ON r.approval_id=a.id WHERE a.id=? AND a.profile_hash=? AND a.candidate_hash=? AND r.approval_id IS NULL")
            .bind(id).bind(&profile.profile_hash).bind(candidate).fetch_optional(&self.pool).await.map_err(storage)?
            .ok_or_else(||invalid("No current independent approval for this exact profile/candidate"))?;
        let binding:Value=serde_json::from_str(row.get::<&str,_>("binding")).map_err(storage)?;
        let expiry=DateTime::parse_from_rfc3339(row.get::<&str,_>("expires_at")).map_err(storage)?;
        let approver:i64=row.get("approver_id");
        let user=sqlx::query("SELECT disabled,auth_version FROM users WHERE id=?").bind(approver).fetch_optional(&self.pool).await.map_err(storage)?
            .ok_or_else(||invalid("Approval identity no longer exists"))?;
        if expiry<=Utc::now() || user.get::<i64,_>("disabled")!=0 || binding["approver_auth_version"]!=user.get::<i64,_>("auth_version")
            || !profile.grants.iter().any(|g|g.user_id==approver&&g.permissions.contains("runtime.approve")) {
            return Err(invalid("Approval expired or authorizing identity/scope was revoked"));
        }
        Ok(binding)
    }
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/workspace",get(workspace))
        .route("/api/runtime/overview",get(overview)).route("/api/runtime/profiles",get(profiles))
        .route("/api/runtime/operations",get(operations).post(submit_operation))
        .route("/api/runtime/operations/:id",get(operation))
        .route("/api/runtime/operations/:id/reconcile",post(reconcile))
        .route("/api/runtime/events",get(events)).route("/api/runtime/events/:id/read",post(read_event))
        .route("/api/runtime/performance",get(performance::performance)).route("/api/runtime/approvals",get(approvals).post(approve))
        .route("/api/runtime/approvals/:id/revoke",post(revoke_approval))
        .merge(publisher::routes())
        .merge(safety::routes())
        .merge(history::routes())
        .merge(maintenance_overview::routes())
}
fn capabilities(state: &AppState) -> Value {
    json!({"profile_operations":!state.runtime.profiles.is_empty(),"native_forecast":true,
        "automatic_handoff":!state.settings.research.enabled&&!state.runtime.profiles.is_empty(),"engine_lifecycle":!state.settings.research.enabled&&!state.runtime.profiles.is_empty(),
        "transport":if state.settings.research.enabled {"isolated_research"} else {"opc_ua"}})
}
async fn workspace(State(state):State<AppState>, user:CurrentUser) -> Json<Value> {
    let scopes:Vec<_> = state.runtime.profiles.values().filter_map(|p| p.grants.iter().find(|g|g.user_id==user.id)
        .map(|g|json!({"profile_hash":p.profile_hash,"target":p.target,
            "permissions":g.permissions.iter().filter(|permission|p.permits(&user,permission)).collect::<Vec<_>>()}))).collect();
    Json(json!({"runtime_enabled":!state.settings.research.enabled,"research_enabled":state.settings.research.enabled,
        "deployment_kind":if state.settings.research.enabled{"isolated_research"}else if state.settings.web.local_test_mode{"local_test_runtime"}else{"field_runtime"},
        "capabilities":capabilities(&state),"user":user,"scopes":scopes,
        "limitations":["Every command requires native instance/epoch attestation and independently acquired application evidence",
            "No ABB commissioning data is inferred; an empty profile registry is observe-only"]}))
}
async fn profiles(State(state):State<AppState>, user:CurrentUser) -> Json<Value> {
    // Endpoint identity is not a credential; profile grants are only disclosed to their subject.
    let values:Vec<_> = state.runtime.profiles.values().filter(|p|p.permits(&user,"runtime.view")).map(|p| {
        let mut visible=p.clone();visible.grants.retain(|grant|grant.user_id==user.id);
        for grant in &mut visible.grants {grant.permissions.retain(|permission|p.permits(&user,permission));}
        visible
    }).collect();
    Json(json!({"profiles":values}))
}
async fn overview(State(state):State<AppState>, user:CurrentUser) -> Result<Json<Value>> {
    let mut profiles = Vec::new();
    for p in state.runtime.profiles.values().filter(|p|p.permits(&user,"runtime.view")) {
        let native=if state.settings.research.enabled {Err(invalid("Isolated research cannot contact field services"))}
            else {transport::status(&state,p).await};
        let observations = state.runtime.observations.read().await;
        let mut tags=RuntimeStore::tags(p);
        tags.insert(p.forecast_tag.clone());tags.insert(p.observation_tag.clone());
        let signals:Vec<_> = tags.iter().filter_map(|tag|observations.get(tag))
            .filter(|sample|sample.source_endpoint==p.endpoint).cloned().collect();
        let fallback_epoch=state.runtime.epoch(&p.profile_hash).await?;
        let native_status=native.as_ref().ok();
        let active_binding=native_status.and_then(|status| {
            let sample_time=status["sample_time"].as_f64()?;
            let prediction=status["prediction_horizon"].as_u64()?;
            let control=status["control_horizon"].as_u64()?;
            if !sample_time.is_finite()||sample_time<=0.0||control==0||prediction<control{return None;}
            let mut binding=p.model_binding.clone();
            binding["sample_time"]=json!(sample_time);binding["prediction_horizon"]=json!(prediction);binding["control_horizon"]=json!(control);
            Some(binding)
        });
        profiles.push(json!({"profile_hash":p.profile_hash,"target":p.target,"equipment_instance_ids":p.equipment_instance_ids,
            "registry_epoch":fallback_epoch,"active_model_binding":active_binding,
            "epoch":native_status.and_then(|s|s["epoch"].as_i64()).unwrap_or(fallback_epoch),
            "instance_id":native_status.and_then(|s|s["instance_id"].as_str()),
            "version":native_status.and_then(|s|s["model_sha256"].as_str()).unwrap_or(&p.model_sha256),"owner_group":p.target.group,
            "health":if native_status.is_some_and(|s|s["healthy"]==true){"good"}else if signals.is_empty(){"unknown"}else{"stale"},
            "blockers":native.as_ref().err().map(|e|vec![e.1.clone()]).unwrap_or_default(),"signals":signals,
            "authority":native_status.and_then(|s|s["mode"].as_str()).unwrap_or("unknown"),
            "arming":native_status.is_some_and(|s|s["armed"]==true),
            "dcs_handoff":native_status.map(|s|s["dcs_handoff"].clone()),"native_status":native_status}));
    }
    Ok(Json(json!({"profiles":profiles,"capabilities":capabilities(&state),"observed_at":Utc::now()})))
}
async fn operations(State(state):State<AppState>, user:CurrentUser) -> Result<Json<Value>> {
    let rows = sqlx::query("SELECT id,profile_hash FROM runtime_operations ORDER BY created_at DESC LIMIT 501")
        .fetch_all(&state.runtime.pool).await.map_err(storage)?;
    let mut records = Vec::new(); let truncated=rows.len()>500;
    for row in rows.into_iter().take(500) {
        if state.runtime.profile(row.get("profile_hash"),&user,"runtime.view").is_ok() {
            if let Some(record)=state.runtime.operation(row.get("id")).await? { records.push(record); }
        }
    }
    Ok(Json(json!({"operations":records,"truncated":truncated})))
}
async fn operation(State(state):State<AppState>, user:CurrentUser, Path(id):Path<String>) -> Result<Json<OperationRecord>> {
    let mut record=state.runtime.operation(&id).await?.ok_or_else(||RuntimeError(StatusCode::NOT_FOUND,"Operation not found".into()))?;
    let profile=state.runtime.profile(&record.profile_hash,&user,"runtime.view")?;
    if record.state=="unknown"&&record.clearance_id.is_none()&&!state.settings.research.enabled {
        if let Ok(intent)=serde_json::from_value::<OperationRequest>(record.request.clone()) {
            if let Ok(response)=transport::original(&state,profile,&intent).await {
                let status=if response["ok"]==true{"accepted"}else{"rejected"};
                state.runtime.reconcile_result(&id,status,&response.to_string()).await?;
                record=state.runtime.operation(&id).await?.ok_or_else(||storage("Lost reconciled operation"))?;
            }
        } else if record.request["kind"]=="install" {
            let approval=record.request["approval"]["approval_id"].as_str().ok_or_else(||invalid("Stored installation approval missing"))?;
            let binding:Option<String>=sqlx::query_scalar("SELECT binding FROM runtime_approvals WHERE id=? AND profile_hash=?")
                .bind(approval).bind(&record.profile_hash).fetch_optional(&state.runtime.pool).await.map_err(storage)?;
            if let Some(binding)=binding {
                let binding:Value=serde_json::from_str(&binding).map_err(storage)?;
                if let (Some(version),Some(model),Some(manifest))=(record.request["version_id"].as_str(),binding["model_sha256"].as_str(),binding["manifest_sha256"].as_str()) {
                    if let Ok(response)=transport::original_installation(&state,profile,&id,version,model,manifest).await {
                        state.runtime.reconcile_result(&id,"accepted",&response.to_string()).await?;
                        record=state.runtime.operation(&id).await?.ok_or_else(||storage("Lost reconciled installation"))?;
                    }
                }
            }
        }
    }
    Ok(Json(record))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReconcileRequest { original_request:OperationRequest }
async fn reconcile(State(state):State<AppState>,user:CurrentUser,_:CsrfVerified,Path(id):Path<String>,Json(body):Json<ReconcileRequest>)->Result<Json<OperationRecord>> {
    let request=body.original_request;
    if id!=request.operation_id{return Err(invalid("Reconciliation path must identify the exact original intent"));}
    let value=serde_json::to_value(&request).map_err(storage)?;
    Ok(Json(state.runtime.reconcile_intent(&user,&id,&request.profile_hash,"runtime.operate",value,request.expires_at).await?))
}
fn write_target<'a>(profile: &'a DeploymentProfile, action: &OperationAction) -> Result<(&'a Variable,&'a str,f64)> {
    let (id,value) = match action { OperationAction::CvTarget{variable_id,value}|OperationAction::OperatingLow{variable_id,value}|OperationAction::OperatingHigh{variable_id,value}=>(variable_id,*value),
        _=>return Err(invalid("Not a numeric operation")) };
    let v=profile.variables.iter().find(|v|&v.id==id).ok_or_else(||invalid("Variable is not in the commissioned profile"))?;
    if !value.is_finite() || value < v.hard_low || value > v.hard_high { return Err(invalid("Requested value exceeds commissioned hard envelope")); }
    let tag=match action {
        OperationAction::CvTarget{..} if v.role=="CV" => v.target_tag.as_deref(),
        OperationAction::OperatingLow{..} if v.role!="DV" => v.low_tag.as_deref(),
        OperationAction::OperatingHigh{..} if v.role!="DV" => v.high_tag.as_deref(),
        _ => None,
    }.ok_or_else(||invalid("Only explicitly commissioned CV targets and operating bounds are writable; manual MV SP is forbidden"))?;
    Ok((v,tag,value))
}
async fn submit_operation(State(state):State<AppState>, user:CurrentUser, _:CsrfVerified,
    Json(request):Json<OperationRequest>) -> Result<Json<OperationRecord>> {
    let _dispatch=state.runtime.dispatch.lock().await;
    let profile=state.runtime.profile(&request.profile_hash,&user,"runtime.operate")?;
    if let Some(existing)=state.runtime.operation(&request.operation_id).await? {
        if existing.actor_id!=user.id || existing.request!=serde_json::to_value(&request).map_err(storage)? {
            return Err(invalid("Operation ID already binds a different intent"));
        }
        return Ok(Json(existing));
    }
    if state.settings.research.enabled {return Err(invalid("Research deployment has no field write capability"));}
    let now=Utc::now();
    if request.operation_id.is_empty()||request.operation_id.len()>128
        ||!request.operation_id.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-'||b==b'_')
        ||request.reason.trim().is_empty()||request.reason.len()>1024
        ||request.expires_at<=now||request.expires_at.signed_duration_since(now).num_seconds()>300 {
        return Err(invalid("Invalid operation ID, reason or expiry"));
    }
    let pending=state.runtime.pending_count(&profile.profile_hash).await?;
    if pending>0&&!matches!(request.action,OperationAction::RecoverMonitor{}|OperationAction::Hold{}|OperationAction::Exit{}|OperationAction::Stop{}) {
        return Err(invalid("Unresolved operation blocks new writes; query the original operation, never resend"));
    }
    if matches!(request.action,OperationAction::Start{}|OperationAction::RecoverMonitor{}|OperationAction::StartCandidate{..}) {
        if request.expected_instance.is_some()||request.expected_version!=profile.model_sha256
            ||request.expected_epoch!=state.runtime.epoch(&profile.profile_hash).await? {
            return Err(invalid("Start target/version/registry epoch changed"));
        }
    } else {
        let status=transport::status(&state,profile).await?;
        if status["model_sha256"]!=request.expected_version||status["epoch"].as_i64()!=Some(request.expected_epoch)
            ||status["instance_id"].as_str()!=request.expected_instance.as_deref()||request.expected_instance.is_none() {
            return Err(invalid("Native instance/version/epoch changed; refresh before any operation"));
        }
        if !matches!(request.action,OperationAction::Monitor{}|OperationAction::Hold{}|OperationAction::Exit{}|OperationAction::Stop{})
            &&status["healthy"]!=true {return Err(invalid("Native acquisition/solve/application health gate is not satisfied"));}
    }
    if matches!(request.action,OperationAction::CvTarget{..}|OperationAction::OperatingLow{..}|OperationAction::OperatingHigh{..}) {
        write_target(profile,&request.action)?;
    }
    if let OperationAction::SwitchVersion{candidate_hash,approval_id}|OperationAction::StartCandidate{candidate_hash,approval_id}=&request.action {
        state.runtime.profile(&request.profile_hash,&user,"runtime.deploy")?;
        state.runtime.approved_binding(profile,candidate_hash,approval_id).await?;
    }
    let actor_version=state.auth.get_user_auth_state(user.id).await.map_err(|_|invalid("Operator identity unavailable"))?
        .ok_or_else(||invalid("Operator identity no longer exists"))?.auth_version;
    let serialized=serde_json::to_string(&request).map_err(storage)?;
    sqlx::query("INSERT INTO runtime_operations(id,profile_hash,actor_id,state,request,detail,created_at,updated_at) VALUES (?,?,?,'executing',?,'Intent committed before native dispatch',?,?)")
        .bind(&request.operation_id).bind(&request.profile_hash).bind(user.id).bind(serialized)
        .bind(now.to_rfc3339()).bind(now.to_rfc3339()).execute(&state.runtime.pool).await.map_err(storage)?;
    let record=state.runtime.operation(&request.operation_id).await?.ok_or_else(||storage("Lost operation"))?;
    let task_state=state.clone();let profile=profile.clone();
    tokio::spawn(async move {
        let still_authorized=task_state.auth.get_user_auth_state(user.id).await.ok().flatten()
            .is_some_and(|current|!current.disabled&&!current.force_password_change&&current.role==user.role&&current.auth_version==actor_version);
        let (status,detail)=if !still_authorized||request.expires_at<=Utc::now() {
            ("rejected","Authorization revoked or intent expired before dispatch".to_owned())
        } else {
            let approval=match &request.action {
                OperationAction::SwitchVersion{candidate_hash,approval_id}|OperationAction::StartCandidate{candidate_hash,approval_id}=>
                    task_state.runtime.approved_binding(&profile,candidate_hash,approval_id).await.map(Some),
                _=>Ok(None),
            };
            match approval {
                Err(error)=>("rejected",error.1),
                Ok(approval)=>match transport::dispatch(&task_state,&profile,&request,approval).await {
                    Ok(response) if response["ok"]==true=>("accepted",response.to_string()),
                    Ok(response)=>("rejected",response.to_string()),
                    Err(error)=>("unknown",error.1),
                }
            }
        };
        if let Err(error)=task_state.runtime.finish(&request.operation_id,status,&detail).await {tracing::error!("{}",error.1);}
        let _=task_state.runtime.event(&request.profile_hash,&request.operation_id,if status=="accepted"{"info"}else{"warning"},&detail).await;
    });
    Ok(Json(record))
}
async fn events(State(state):State<AppState>, user:CurrentUser) -> Result<Json<Value>> {
    let rows=sqlx::query("SELECT e.*,r.read_at,c.condition_key,c.active,c.cleared_at,c.last_evaluated FROM runtime_events e LEFT JOIN runtime_event_reads r ON e.id=r.event_id AND r.user_id=? LEFT JOIN runtime_condition_events c ON c.event_id=e.id ORDER BY e.id DESC LIMIT 501")
        .bind(user.id).fetch_all(&state.runtime.pool).await.map_err(storage)?;
    let truncated=rows.len()>500; let mut events=Vec::new();
    for row in rows.into_iter().take(500) {
        let hash:String=row.get("profile_hash");
        if state.runtime.profile(&hash,&user,"runtime.view").is_err() {continue;}
        events.push(json!({"id":row.get::<i64,_>("id"),"profile_hash":hash,"source":row.get::<String,_>("source"),
            "severity":row.get::<String,_>("severity"),"condition":row.get::<String,_>("condition"),
            "first_seen":row.get::<String,_>("first_seen"),"last_seen":row.get::<String,_>("last_seen"),
            "read_at":row.get::<Option<String>,_>("read_at"),"condition_key":row.get::<Option<String>,_>("condition_key"),
            "condition_active":row.get::<Option<i64>,_>("active").map(|active|active!=0),
            "cleared_at":row.get::<Option<String>,_>("cleared_at"),"last_evaluated":row.get::<Option<String>,_>("last_evaluated")}));
    }
    Ok(Json(json!({"events":events,"truncated":truncated,"service_status":"available",
        "external_alarm_status":"unknown","acknowledgement_semantics":"read_only_not_dcs_acknowledgement"})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadEvent { note:String }
async fn read_event(State(state):State<AppState>, user:CurrentUser, _:CsrfVerified, Path(id):Path<i64>, Json(request):Json<ReadEvent>) -> Result<Json<Value>> {
    if request.note.len()>2048 {return Err(invalid("Note exceeds limit"));}
    let hash:String=sqlx::query_scalar("SELECT profile_hash FROM runtime_events WHERE id=?").bind(id).fetch_optional(&state.runtime.pool).await.map_err(storage)?
        .ok_or_else(||RuntimeError(StatusCode::NOT_FOUND,"Event not found".into()))?;
    state.runtime.profile(&hash,&user,"runtime.view")?;
    let now=Utc::now().to_rfc3339();
    sqlx::query("INSERT INTO runtime_event_reads(event_id,user_id,read_at,note) VALUES (?,?,?,?) ON CONFLICT(event_id,user_id) DO UPDATE SET read_at=excluded.read_at,note=excluded.note")
        .bind(id).bind(user.id).bind(&now).bind(request.note).execute(&state.runtime.pool).await.map_err(storage)?;
    Ok(Json(json!({"id":id,"read_at":now,"interlocks_changed":false})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalRequest { profile_hash:String,candidate_hash:String,expires_at:DateTime<Utc> }
async fn approve(State(state):State<AppState>,user:CurrentUser,_:CsrfVerified,Json(request):Json<ApprovalRequest>) -> Result<Json<Value>> {
    let profile=state.runtime.profile(&request.profile_hash,&user,"runtime.approve")?;
    if request.expires_at<=Utc::now() || request.expires_at.signed_duration_since(Utc::now()).num_days()>30 {return Err(invalid("Approval expiry must be within 30 days"));}
    let bundle=state.research.version_bundle(&request.candidate_hash).await.map_err(|e|RuntimeError(e.0,e.1))?;
    bundle.verify(&request.candidate_hash).map_err(|e|RuntimeError(e.0,e.1))?;
    let manifest:Value=serde_json::from_str(&bundle.manifest_json).map_err(storage)?;
    let author=manifest["author_user_id"].as_i64().ok_or_else(||invalid("Candidate lacks authenticated author identity"))?;
    let realm=publisher::identity_realm(&state.runtime).await?;
    if manifest["author_identity_realm"]!=realm{return Err(invalid("Candidate author identity realm is not this commissioned identity authority"));}
    if author==user.id {return Err(RuntimeError(StatusCode::FORBIDDEN,"Candidate author cannot approve their own work".into()));}
    let instances: BTreeSet<String>=serde_json::from_value(manifest["equipment_instance_ids"].clone()).map_err(|_|invalid("Candidate lacks exact equipment instance binding"))?;
    if manifest["deployment_profile_hash"]!=profile.profile_hash || manifest["target"]!=serde_json::to_value(&profile.target).map_err(storage)?
        || instances!=profile.equipment_instance_ids || manifest["operating_envelope"]!=profile.operating_envelope {
        return Err(invalid("Candidate target, equipment, operating envelope or profile differs from commissioned authority"));
    }
    verify_candidate_model(profile,&bundle.model_json)?;
    if profile.commissioning_kind=="field" && manifest["holdout"]["independent"]!=true {
        return Err(invalid("Field approval requires independent evidence; synthetic/exploratory research is not factory qualification"));
    }
    let auth_state=state.auth.get_user_auth_state(user.id).await.map_err(|_|storage("Approval identity unavailable"))?
        .ok_or_else(||invalid("Approver identity no longer exists"))?;
    let binding=json!({"profile_hash":profile.profile_hash,"candidate_hash":request.candidate_hash,
        "model_sha256":bundle.model_sha256,"manifest_sha256":bundle.manifest_sha256,"target":profile.target,
        "equipment_instance_ids":profile.equipment_instance_ids,"operating_envelope":profile.operating_envelope,
        "author_id":author,"approver_id":user.id,"approver_auth_version":auth_state.auth_version,"expires_at":request.expires_at,
        "author_identity_realm":realm,"approver_identity_realm":realm,"kind":"manual_review"});
    let encoded=serde_json::to_string(&binding).map_err(storage)?;let id=digest(encoded.as_bytes());
    sqlx::query("INSERT OR IGNORE INTO runtime_approvals(id,profile_hash,candidate_hash,author_id,approver_id,expires_at,binding) VALUES (?,?,?,?,?,?,?)")
        .bind(&id).bind(&profile.profile_hash).bind(&request.candidate_hash).bind(author).bind(user.id)
        .bind(request.expires_at.to_rfc3339()).bind(encoded).execute(&state.runtime.pool).await.map_err(storage)?;
    state.runtime.event(&profile.profile_hash,&id,"info","Independent candidate approval recorded; no install/start/arm/Engage performed").await?;
    Ok(Json(json!({"approval_id":id,"binding":binding,"installed":false,"started":false,"armed":false})))
}

async fn approvals(State(state):State<AppState>,user:CurrentUser)->Result<Json<Value>> {
    let rows=sqlx::query("SELECT a.*,r.revoked_at FROM runtime_approvals a LEFT JOIN runtime_approval_revocations r ON a.id=r.approval_id ORDER BY a.expires_at DESC LIMIT 501")
        .fetch_all(&state.runtime.pool).await.map_err(storage)?;
    let truncated=rows.len()>500;let mut values=Vec::new();
    for row in rows.into_iter().take(500) {
        if state.runtime.profile(row.get("profile_hash"),&user,"runtime.view").is_err(){continue;}
        let binding:Value=serde_json::from_str(row.get::<&str,_>("binding")).map_err(storage)?;
        values.push(json!({"approval_id":row.get::<String,_>("id"),"binding":binding,"revoked_at":row.get::<Option<String>,_>("revoked_at")}));
    }
    Ok(Json(json!({"approvals":values,"truncated":truncated})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Revocation { reason:String }
async fn revoke_approval(State(state):State<AppState>,user:CurrentUser,_:CsrfVerified,Path(id):Path<String>,Json(request):Json<Revocation>)->Result<Json<Value>> {
    if request.reason.trim().is_empty()||request.reason.len()>1024{return Err(invalid("Revocation reason required"));}
    let profile:String=sqlx::query_scalar("SELECT profile_hash FROM runtime_approvals WHERE id=?").bind(&id)
        .fetch_optional(&state.runtime.pool).await.map_err(storage)?.ok_or_else(||invalid("Approval not found"))?;
    state.runtime.profile(&profile,&user,"runtime.approve")?;
    sqlx::query("INSERT OR IGNORE INTO runtime_approval_revocations VALUES (?,?,?,?)")
        .bind(&id).bind(user.id).bind(Utc::now().to_rfc3339()).bind(&request.reason).execute(&state.runtime.pool).await.map_err(storage)?;
    state.runtime.event(&profile,&id,"warning","Approval revoked; no new installation or handoff is authorized").await?;
    Ok(Json(json!({"approval_id":id,"revoked":true,"controller_stopped":false})))
}

pub async fn canonicalize_profiles(input:&str,output:&str)->std::result::Result<(),String> {
    let bytes=tokio::fs::read(input).await.map_err(|e|e.to_string())?;
    if bytes.len()>4*1024*1024{return Err("Profile file exceeds 4 MiB".into());}
    let mut profiles:Vec<DeploymentProfile>=serde_json::from_slice(&bytes).map_err(|e|e.to_string())?;
    for profile in &mut profiles {profile.profile_hash=profile.calculated_hash()?;profile.validate()?;}
    let encoded=serde_json::to_vec_pretty(&profiles).map_err(|e|e.to_string())?;
    use tokio::io::AsyncWriteExt;
    let mut file=tokio::fs::OpenOptions::new().write(true).create_new(true).open(output).await.map_err(|e|e.to_string())?;
    file.write_all(&encoded).await.map_err(|e|e.to_string())?;
    file.sync_all().await.map_err(|e|e.to_string())
}

fn verify_candidate_model(profile:&DeploymentProfile,encoded:&str)->Result<()> {
    let model:Value=serde_json::from_str(encoded).map_err(|_|invalid("Invalid candidate model"))?;
    if model["metadata"]["name"]!=profile.engine_id || model["tuning"]["sample_time"]!=profile.model_binding["sample_time"] {
        return Err(invalid("Candidate changed engine identity or commissioned sample time"));
    }
    for (role,key) in [("CV","cvs"),("MV","mvs"),("DV","dvs")] {
        let variables=model["variables"][key].as_array().ok_or_else(||invalid("Missing candidate variable group"))?;
        let binding=profile.model_binding[key].as_array().ok_or_else(||invalid("Missing profile variable binding"))?;
        if variables.len()!=binding.len() || variables.len()!=profile.variables.iter().filter(|v|v.role==role).count() {
            return Err(invalid("Candidate variable dimensions differ from commissioned profile"));
        }
        for (candidate,bound) in variables.iter().zip(binding) {
            let variable=profile.variables.iter().find(|v|v.role==role&&candidate["name"]==v.id)
                .ok_or_else(||invalid("Candidate variable identity differs from commissioned profile"))?;
            if candidate["name"]!=bound["name"] || candidate["units"]!=variable.unit || candidate["node_ids"]!=bound["node_ids"]
                || (role!="DV"&&(candidate["limits"]["low_low"]!=variable.hard_low || candidate["limits"]["high_high"]!=variable.hard_high))
                || (role=="MV"&&candidate["max_move"].as_f64().is_none_or(|value|!value.is_finite()||value<=0.0||variable.max_move.is_none_or(|bound|value>bound)))
                || candidate["limits"]["low"].as_f64().is_none_or(|v|v<variable.hard_low)
                || candidate["limits"]["high"].as_f64().is_none_or(|v|v>variable.hard_high) {
                return Err(invalid("Candidate may not change tags, units, hard limits or input order, or exceed commissioned max_move"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn extract_operation(action: Value) -> Result<Json<OperationRequest>, axum::extract::rejection::JsonRejection> {
        use axum::{body::Body, extract::FromRequest, http::Request};
        let body=json!({
            "operation_id":"operator_intent","profile_hash":"a".repeat(64),
            "expected_version":"b".repeat(64),"expected_epoch":7,"expected_instance":"instance_1",
            "expires_at":"2030-01-01T00:00:00Z","reason":"Explicit operator request","action":action
        });
        let request=Request::builder().method("POST").uri("/api/runtime/operations")
            .header("content-type","application/json").body(Body::from(body.to_string())).unwrap();
        Json::<OperationRequest>::from_request(request,&()).await
    }

    #[tokio::test]
    async fn operation_schema_rejects_mv_and_lifecycle_bypasses() {
        for kind in ["start","recover_monitor","monitor","arm","engage","hold","exit","stop"] {
            for extra in [
                json!({"variable_id":"feed"}),json!({"value":5}),json!({"tag":"arbitrary"}),
                json!({"variable_id":"feed","value":5})
            ] {
                let mut action=json!({"kind":kind});
                action.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
                let error=extract_operation(action.clone()).await.unwrap_err();
                assert_eq!(error.status(),StatusCode::UNPROCESSABLE_ENTITY,"{action}");
            }
        }
        for kind in ["mv_sp","restart","auto_handoff","raw_write"] {
            for action in [json!({"kind":kind}),json!({"kind":kind,"variable_id":"feed","value":5})] {
                assert_eq!(extract_operation(action.clone()).await.unwrap_err().status(),
                    StatusCode::UNPROCESSABLE_ENTITY,"{action}");
            }
        }
        let action=json!({"kind":"cv_target","variable_id":"fineness","value":5,"tag":"arbitrary"});
        assert_eq!(extract_operation(action).await.unwrap_err().status(),StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn explicit_lifecycle_operations_preserve_the_wire_contract() {
        for kind in ["start","recover_monitor","monitor","arm","engage","hold","exit","stop"] {
            let action=json!({"kind":kind});
            let Json(request)=extract_operation(action.clone()).await.unwrap();
            assert_eq!(serde_json::to_value(&request.action).unwrap(),action);
        }
    }
    #[tokio::test]
    async fn committed_intent_survives_without_acknowledgement() {
        let store=RuntimeStore::open("sqlite::memory:",vec![],true).await.unwrap();
        sqlx::query("INSERT INTO runtime_operations VALUES ('one','profile',1,'executing','{}','pending','now','now')").execute(&store.pool).await.unwrap();
        assert_eq!(store.operation("one").await.unwrap().unwrap().state,"executing");
        store.finish("one","unknown","transport interrupted").await.unwrap();
        store.finish("one","accepted","late handler cannot override unresolved result").await.unwrap();
        assert_eq!(store.operation("one").await.unwrap().unwrap().state,"unknown");
        assert!(sqlx::query("INSERT INTO runtime_operations VALUES ('one','profile',1,'executing','{}','retry','now','now')").execute(&store.pool).await.is_err());
    }
}

pub(crate) fn authorize_legacy_read(state:&AppState,user:&CurrentUser)->Result<()> {
    if state.settings.research.enabled||user.role!=crate::auth::Role::Admin||state.runtime.profiles.is_empty()
        ||!state.runtime.profiles.values().all(|profile|profile.permits(user,"runtime.maintenance")) {
        return Err(RuntimeError(StatusCode::FORBIDDEN,"Legacy unscoped reads require explicit maintenance scope; use commissioned runtime views".into()));
    }
    Ok(())
}

impl RuntimeStore {
    pub(crate) async fn worker_connection(&self)->Value {
        let worker=self.worker_connection.read().await;
        json!({"reported_connected":worker.reported_connected,"generation":worker.generation,"generation_scope":"current_hmi_process",
            "observed_at":worker.observed_at,"reason":worker.reason})
    }
}
