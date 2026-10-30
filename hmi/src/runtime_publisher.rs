//! Independently granted tuning publication. A policy grant is not individual candidate review.
//! Queue claims and native intents are durable before transport; no recovery path replays a claim.
use std::collections::{BTreeMap, BTreeSet};
use axum::{extract::{Path, State}, http::StatusCode, routing::{get, post}, Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::Row;
use crate::{auth::{CsrfVerified, CurrentUser, Role}, control_versions::FrozenVersionBundle, state::AppState};
use super::{digest, invalid, storage, DeploymentProfile, OperationAction, OperationRequest, Result, RuntimeError, RuntimeStore, Target};

const MANAGE: &str = "runtime.publish";
const GRANT: &str = "runtime.publish.grant";
const MAX_POLICIES: i64 = 64;
const POLL_SECONDS: u64 = 5;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NumericBound {
    pub pointer: String,
    pub minimum: f64,
    pub maximum: f64,
    pub max_delta: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EnvelopeBound {
    pub tag: String,
    pub minimum: f64,
    pub maximum: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PolicySpec {
    pub profile_hash: String,
    pub target: Target,
    pub baseline_candidate_hash: String,
    pub baseline_model_sha256: String,
    pub source_policy_id: String,
    pub source_policy_revision: i64,
    pub source_policy_sha256: String,
    pub source_study_id: String,
    pub allowlist: Vec<NumericBound>,
    pub operating_envelope: Vec<EnvelopeBound>,
    pub minimum_observation_ms: u64,
    pub minimum_good_samples: u64,
    pub cooldown_ms: u64,
    pub max_changes: u64,
    pub expires_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PolicyRecord {
    pub policy_id: String,
    pub revision: i64,
    pub revision_sha256: String,
    pub state: String,
    pub spec: PolicySpec,
    pub created_by: i64,
    pub spec_author_id: i64,
    pub revision_by: i64,
    pub grantor_id: Option<i64>,
    pub grantor_auth_version: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PublisherJob {
    pub job_id: String,
    pub policy_id: String,
    pub policy_revision: i64,
    pub policy_revision_sha256: String,
    pub profile_hash: String,
    pub candidate_hash: String,
    pub model_sha256: String,
    pub manifest_sha256: String,
    pub expected_candidate_hash: String,
    pub expected_instance: String,
    pub expected_version: String,
    pub expected_epoch: i64,
    pub expires_at: DateTime<Utc>,
    pub state: String,
    pub operation_id: String,
    pub created_at: DateTime<Utc>,
    pub detail: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PublisherDecision {
    pub id: i64,
    pub policy_id: String,
    pub revision: i64,
    pub candidate_hash: Option<String>,
    pub outcome: String,
    pub reason: String,
    pub created_at: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreatePolicy { policy_id: String, expected_revision: i64, spec: PolicySpec, reason: String }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevisePolicy { expected_revision: i64, spec: PolicySpec, reason: String }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangeState { expected_revision: i64, state: String, reason: String }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueueCandidate { expected_revision: i64, candidate_hash: String, reason: String }

/// Native attestation and actual producer timestamps, never scheduler elapsed time.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PublicationObservation {
    pub instance: String,
    pub version: String,
    pub epoch: i64,
    pub sequence: u64,
    pub sampled_at: DateTime<Utc>,
    pub healthy: bool,
    pub armed: bool,
    pub engaged: bool,
    pub unresolved_operations: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ObservationWindow {
    last: Option<PublicationObservation>,
    good_samples: u64,
    good_ms: u64,
}

fn valid_hash(value: &str) -> bool {
    value.len()==64 && value.bytes().all(|b|b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn reason(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len()>1024 { return Err(invalid("A bounded publication reason is required")); }
    Ok(())
}
fn tuning_pointer(pointer: &str) -> bool {
    let mut parts=pointer.split('/');
    match (parts.next(),parts.next(),parts.next(),parts.next(),parts.next(),parts.next()) {
        (Some(""),Some("variables"),Some("cvs"),Some(index),Some(field),None) =>
            canonical_index(index) && matches!(field,"weight"|"alpha"|"slack_weight"),
        (Some(""),Some("variables"),Some("mvs"),Some(index),Some(field),None) =>
            canonical_index(index) && matches!(field,"weight_r"|"target_weight"),
        (Some(""),Some("tuning"),Some(field),None,None,None) => matches!(field,"prediction_horizon"|"control_horizon"),
        _ => false,
    }
}
fn canonical_index(index:&str)->bool {
    (index=="0" || !index.starts_with('0')) && index.bytes().all(|byte|byte.is_ascii_digit()) && index.parse::<usize>().is_ok()
}
/// Skip granted numeric leaves without cloning response matrices or constructing a JSON patch.
fn unchanged_except(before:&Value,after:&Value,pointers:&BTreeSet<&str>,path:&mut String)->bool {
    if pointers.contains(path.as_str()) {return true;}
    if !pointers.iter().any(|pointer|pointer.starts_with(path.as_str()) && pointer.as_bytes().get(path.len())==Some(&b'/')) {
        return before==after;
    }
    let length=path.len();
    match (before,after) {
        (Value::Object(old),Value::Object(new)) if old.len()==new.len() => {
            for (key,value) in old {
                let Some(next)=new.get(key) else {return false;};
                path.push('/');
                for character in key.chars() {
                    match character {'~'=>path.push_str("~0"),'/'=>path.push_str("~1"),other=>path.push(other)}
                }
                let same=unchanged_except(value,next,pointers,path);
                path.truncate(length);
                if !same {return false;}
            }
            true
        }
        (Value::Array(old),Value::Array(new)) if old.len()==new.len() => {
            use std::fmt::Write as _;
            for (index,(value,next)) in old.iter().zip(new).enumerate() {
                if write!(path,"/{index}").is_err() {return false;}
                let same=unchanged_except(value,next,pointers,path);
                path.truncate(length);
                if !same {return false;}
            }
            true
        }
        _ => before==after,
    }
}
impl PolicySpec {
    /// Compare complete parsed model bytes, not a client-supplied patch or claimed delta.
    pub(crate) fn validate_transition(&self, before:&Value, after:&Value)->Result<()> {
        if self.allowlist.is_empty() || self.allowlist.len()>128 { return Err(invalid("Explicit bounded tuning pointers required")); }
        let mut seen=BTreeSet::new();
        let mut changed=false;
        for bound in &self.allowlist {
            if !tuning_pointer(&bound.pointer) || !seen.insert(bound.pointer.as_str())
                || ![bound.minimum,bound.maximum,bound.max_delta].iter().all(|n|n.is_finite())
                || bound.minimum>bound.maximum || bound.max_delta<=0.0 {
                return Err(invalid("Only exact numeric tuning weights, alphas and horizons may be granted"));
            }
            let old=before.pointer(&bound.pointer).and_then(Value::as_f64)
                .ok_or_else(||invalid("Allowlisted baseline value must already be numeric"))?;
            let new=after.pointer(&bound.pointer).and_then(Value::as_f64)
                .ok_or_else(||invalid("Allowlisted candidate value must remain numeric"))?;
            if new<bound.minimum || new>bound.maximum || (new-old).abs()>bound.max_delta {
                return Err(invalid(format!("Candidate exceeds absolute bound or per-transition delta: {}",bound.pointer)));
            }
            if (bound.pointer.ends_with("/alpha") && !(0.0..=1.0).contains(&new))
                || (!bound.pointer.ends_with("/alpha") && new<0.0)
                || (bound.pointer.starts_with("/tuning/") && (new<1.0 || new.fract()!=0.0)) {
                return Err(invalid("Tuning value violates its numeric domain"));
            }
            changed |= before.pointer(&bound.pointer)!=after.pointer(&bound.pointer);
        }
        if !unchanged_except(before,after,&seen,&mut String::new()) {
            return Err(invalid("Candidate changes non-granted model bytes: physics, tags, Ts, identity and limits are immutable"));
        }
        if after["tuning"]["control_horizon"].as_u64().zip(after["tuning"]["prediction_horizon"].as_u64())
            .is_some_and(|(control,prediction)|control>prediction) {
            return Err(invalid("Control horizon exceeds prediction horizon"));
        }
        if !changed {return Err(invalid("Candidate makes no permitted tuning change"));}
        Ok(())
    }
    fn validate(&self,profile:&DeploymentProfile,baseline:&FrozenVersionBundle)->Result<()> {
        if self.profile_hash!=profile.profile_hash || self.target!=profile.target
            || self.baseline_candidate_hash!=baseline.version_id || self.baseline_model_sha256!=baseline.model_sha256
            || !valid_hash(&self.source_policy_sha256) || self.source_policy_revision<1
            || self.source_policy_id.is_empty() || self.source_policy_id.len()>128
            || self.source_study_id.is_empty() || self.source_study_id.len()>128
            || !(1..=100).contains(&self.max_changes) || !(2..=1_000_000).contains(&self.minimum_good_samples)
            || !(1_000..=2_592_000_000).contains(&self.minimum_observation_ms)
            || !(1_000..=2_592_000_000).contains(&self.cooldown_ms)
            || self.expires_at<=Utc::now() || self.expires_at.signed_duration_since(Utc::now()).num_days()>30 {
            return Err(invalid("Publication scope, immutable baseline, lineage or finite budget is invalid"));
        }
        let baseline_model:Value=serde_json::from_str(&baseline.model_json).map_err(storage)?;
        if self.allowlist.is_empty() || self.allowlist.len()>128 {return Err(invalid("Explicit numerical allowlist required"));}
        let mut seen=BTreeSet::new();
        for bound in &self.allowlist {
            let value=baseline_model.pointer(&bound.pointer).and_then(Value::as_f64)
                .ok_or_else(||invalid("Tuning pointer must exist as a number in the immutable baseline"))?;
            if !tuning_pointer(&bound.pointer) || !seen.insert(&bound.pointer)
                || ![bound.minimum,bound.maximum,bound.max_delta].iter().all(|n|n.is_finite())
                || bound.minimum>value || bound.maximum<value || bound.max_delta<=0.0 {
                return Err(invalid("Baseline is outside the explicit numerical grant"));
            }
        }
        let tags=RuntimeStore::tags(profile); let mut seen=BTreeSet::new();
        if self.operating_envelope.is_empty() || self.operating_envelope.len()>128 {
            return Err(invalid("An explicit measured operating envelope is required"));
        }
        for bound in &self.operating_envelope {
            if !tags.contains(&bound.tag) || !seen.insert(&bound.tag)
                || !bound.minimum.is_finite() || !bound.maximum.is_finite() || bound.minimum>bound.maximum {
                return Err(invalid("Envelope must use unique commissioned observable tags and finite bounds"));
            }
        }
        for variable in &profile.variables {
            let bound=self.operating_envelope.iter().find(|bound|bound.tag==variable.pv_tag)
                .ok_or_else(||invalid("Operating envelope must explicitly bound every commissioned variable PV"))?;
            if bound.minimum<variable.hard_low || bound.maximum>variable.hard_high {
                return Err(invalid("Publication envelope cannot exceed commissioned hard bounds"));
            }
        }
        super::verify_candidate_model(profile,&baseline.model_json)
    }
}
impl PublicationObservation {
    fn eligible(&self)->bool {
        self.healthy && self.armed && self.engaged && !self.unresolved_operations && self.sequence%2==0
            && !self.instance.is_empty() && valid_hash(&self.version)
    }
}
impl PublisherJob {
    pub(crate) fn validate_execution(&self,policy:&PolicyRecord,observed:&PublicationObservation,now:DateTime<Utc>)->Result<()> {
        if self.state!="queued" || policy.state!="enabled" || policy.policy_id!=self.policy_id
            || policy.revision!=self.policy_revision || policy.revision_sha256!=self.policy_revision_sha256
            || policy.spec.profile_hash!=self.profile_hash || now>=self.expires_at || now>=policy.spec.expires_at {
            return Err(invalid("Frozen job expired, was claimed, or its independent policy grant changed"));
        }
        if !observed.eligible() || observed.instance!=self.expected_instance
            || observed.version!=self.expected_version || observed.epoch!=self.expected_epoch {
            return Err(invalid("Frozen instance/version/epoch or healthy explicitly armed Engage authority changed"));
        }
        Ok(())
    }
    pub(crate) fn reconciled_state(&self,original:&super::OperationRecord)->Option<&'static str> {
        if original.operation_id!=self.operation_id || original.profile_hash!=self.profile_hash {return None;}
        match original.state.as_str() {
            "accepted"=>Some("accepted"),
            "rejected"=>Some("rejected"),
            "unknown" if !original.blocking
                && original.clearance_id.as_deref().is_some_and(|id|!id.is_empty())
                && original.cleared_at.is_some() && original.clearance_evidence.is_some() => Some("cleared_unknown"),
            _=>None,
        }
    }
}
impl ObservationWindow {
    fn observe(&mut self,next:PublicationObservation,maximum_gap_ms:u64) {
        if !next.eligible() { *self=Self::default(); return; }
        if let Some(last)=&self.last {
            if last.instance==next.instance && last.version==next.version && last.epoch==next.epoch {
                if next.sequence==last.sequence && next.sampled_at==last.sampled_at {return;}
                let elapsed=next.sampled_at.signed_duration_since(last.sampled_at).num_milliseconds();
                if next.sequence>last.sequence && elapsed>0 && elapsed as u64<=maximum_gap_ms {
                    self.good_samples=self.good_samples.saturating_add(1);
                    self.good_ms=self.good_ms.saturating_add(elapsed as u64);
                    self.last=Some(next);
                    return;
                }
            }
        }
        self.last=Some(next); self.good_samples=1; self.good_ms=0;
    }
    fn ready(&self,spec:&PolicySpec,attempts:u64)->bool {
        attempts<spec.max_changes && self.good_samples>=spec.minimum_good_samples
            && self.good_ms>=spec.minimum_observation_ms.max(spec.cooldown_ms)
    }
}

/// Called once on RuntimeStore initialization. Restart invalidates observations and never resumes a queue.
pub(crate) async fn init(store:&RuntimeStore)->Result<()> {
    for statement in [
        "CREATE TABLE IF NOT EXISTS publisher_identity (id INTEGER PRIMARY KEY CHECK(id=1), realm TEXT NOT NULL)",
        "CREATE TABLE IF NOT EXISTS publisher_revisions (policy_id TEXT NOT NULL, revision INTEGER NOT NULL, record TEXT NOT NULL, PRIMARY KEY(policy_id,revision))",
        "CREATE TABLE IF NOT EXISTS publisher_heads (policy_id TEXT PRIMARY KEY, revision INTEGER NOT NULL)",
        "CREATE TABLE IF NOT EXISTS publisher_jobs (job_id TEXT PRIMARY KEY, policy_id TEXT NOT NULL, candidate_hash TEXT NOT NULL, state TEXT NOT NULL, detail TEXT NOT NULL, dispatched INTEGER NOT NULL DEFAULT 0, record TEXT NOT NULL, UNIQUE(policy_id,candidate_hash))",
        "CREATE TABLE IF NOT EXISTS publisher_windows (policy_id TEXT PRIMARY KEY, revision INTEGER NOT NULL, record TEXT NOT NULL)",
        "CREATE TABLE IF NOT EXISTS publisher_decisions (id INTEGER PRIMARY KEY AUTOINCREMENT, decision_key TEXT UNIQUE NOT NULL, policy_id TEXT NOT NULL, revision INTEGER NOT NULL, candidate_hash TEXT, outcome TEXT NOT NULL, reason TEXT NOT NULL, created_at TEXT NOT NULL)",
        "CREATE TABLE IF NOT EXISTS publisher_scheduler (id INTEGER PRIMARY KEY CHECK(id=1), observed_at TEXT, status TEXT NOT NULL)",
    ] {sqlx::query(statement).execute(&store.pool).await.map_err(storage)?;}
    sqlx::query("INSERT OR IGNORE INTO publisher_identity VALUES (1,lower(hex(randomblob(32))))")
        .execute(&store.pool).await.map_err(storage)?;
    sqlx::query("INSERT OR IGNORE INTO publisher_scheduler VALUES (1,NULL,'not_started')")
        .execute(&store.pool).await.map_err(storage)?;
    // A research-only HMI may share the authentication database; it must not recover a live publisher.
    if !store.runtime_enabled {return Ok(());}
    recover(store).await
}
async fn recover(store:&RuntimeStore)->Result<()> {
    let mut transaction=store.pool.begin().await.map_err(storage)?;
    for profile_hash in store.profiles.keys() {
        sqlx::query("UPDATE publisher_jobs SET state='unknown',detail='Restart crossed a durable dispatch claim; query the original operation, never resend' WHERE state='dispatching' AND json_extract(record,'$.profile_hash')=?")
            .bind(profile_hash).execute(&mut *transaction).await.map_err(storage)?;
        sqlx::query("UPDATE publisher_jobs SET state='rejected',detail='Restart invalidated the frozen queue authority; this candidate is not retried' WHERE state='queued' AND json_extract(record,'$.profile_hash')=?")
            .bind(profile_hash).execute(&mut *transaction).await.map_err(storage)?;
        sqlx::query("DELETE FROM publisher_windows WHERE policy_id IN (SELECT h.policy_id FROM publisher_heads h JOIN publisher_revisions r ON h.policy_id=r.policy_id AND h.revision=r.revision WHERE json_extract(r.record,'$.spec.profile_hash')=?)")
            .bind(profile_hash).execute(&mut *transaction).await.map_err(storage)?;
    }
    sqlx::query("INSERT INTO publisher_scheduler VALUES (1,NULL,'not_started') ON CONFLICT(id) DO UPDATE SET observed_at=NULL,status='not_started'")
        .execute(&mut *transaction).await.map_err(storage)?;
    transaction.commit().await.map_err(storage)
}
pub(crate) async fn identity_realm(store:&RuntimeStore)->Result<String> {
    sqlx::query_scalar("SELECT realm FROM publisher_identity WHERE id=1").fetch_one(&store.pool).await.map_err(storage)
}
async fn latest(store:&RuntimeStore,id:&str)->Result<PolicyRecord> {
    let encoded:String=sqlx::query_scalar("SELECT r.record FROM publisher_revisions r JOIN publisher_heads h ON h.policy_id=r.policy_id AND h.revision=r.revision WHERE r.policy_id=?")
        .bind(id).fetch_optional(&store.pool).await.map_err(storage)?
        .ok_or_else(||RuntimeError(StatusCode::NOT_FOUND,"Publication policy not found".into()))?;
    serde_json::from_str(&encoded).map_err(storage)
}
async fn policies(store:&RuntimeStore)->Result<Vec<PolicyRecord>> {
    let rows:Vec<String>=sqlx::query_scalar("SELECT r.record FROM publisher_revisions r JOIN publisher_heads h ON h.policy_id=r.policy_id AND h.revision=r.revision ORDER BY r.policy_id")
        .fetch_all(&store.pool).await.map_err(storage)?;
    rows.iter().map(|record|serde_json::from_str(record).map_err(storage)).collect()
}
async fn save_revision(store:&RuntimeStore,mut record:PolicyRecord,expected:i64)->Result<PolicyRecord> {
    record.revision=expected+1;
    record.revision_sha256.clear();
    record.revision_sha256=digest(&serde_json::to_vec(&record).map_err(storage)?);
    let mut transaction=store.pool.begin().await.map_err(storage)?;
    if expected==0 {
        let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM publisher_heads")
            .fetch_one(&mut *transaction).await.map_err(storage)?;
        if count>=MAX_POLICIES {return Err(invalid("Publication policy capacity reached"));}
        let inserted=sqlx::query("INSERT OR IGNORE INTO publisher_heads VALUES (?,?)")
            .bind(&record.policy_id).bind(record.revision).execute(&mut *transaction).await.map_err(storage)?;
        if inserted.rows_affected()!=1 {return Err(invalid("Policy already exists; revisions require compare-and-swap"));}
    } else {
        let updated=sqlx::query("UPDATE publisher_heads SET revision=? WHERE policy_id=? AND revision=?")
            .bind(record.revision).bind(&record.policy_id).bind(expected).execute(&mut *transaction).await.map_err(storage)?;
        if updated.rows_affected()!=1 {return Err(invalid("Publication policy revision changed; stale mutation rejected"));}
    }
    sqlx::query("INSERT INTO publisher_revisions VALUES (?,?,?)").bind(&record.policy_id).bind(record.revision)
        .bind(serde_json::to_string(&record).map_err(storage)?).execute(&mut *transaction).await.map_err(storage)?;
    sqlx::query("UPDATE publisher_jobs SET state='rejected',detail='Policy revision, enabled state or independent grant changed before execution' WHERE policy_id=? AND state='queued'")
        .bind(&record.policy_id).execute(&mut *transaction).await.map_err(storage)?;
    sqlx::query("DELETE FROM publisher_windows WHERE policy_id=?").bind(&record.policy_id)
        .execute(&mut *transaction).await.map_err(storage)?;
    transaction.commit().await.map_err(storage)?;
    Ok(record)
}
async fn record_decision(store:&RuntimeStore,policy:&PolicyRecord,candidate:Option<&str>,outcome:&str,message:&str)->Result<()> {
    let key=digest(json!([policy.policy_id,policy.revision,candidate,outcome,message]).to_string().as_bytes());
    sqlx::query("INSERT OR IGNORE INTO publisher_decisions (decision_key,policy_id,revision,candidate_hash,outcome,reason,created_at) VALUES (?,?,?,?,?,?,?)")
        .bind(key).bind(&policy.policy_id).bind(policy.revision).bind(candidate).bind(outcome).bind(message)
        .bind(Utc::now().to_rfc3339()).execute(&store.pool).await.map_err(storage)?;
    Ok(())
}
async fn jobs(store:&RuntimeStore,policy_id:Option<&str>)->Result<Vec<PublisherJob>> {
    let rows=sqlx::query("SELECT record,state,detail FROM publisher_jobs WHERE (? IS NULL OR policy_id=?) ORDER BY rowid DESC LIMIT 501")
        .bind(policy_id).bind(policy_id).fetch_all(&store.pool).await.map_err(storage)?;
    rows.into_iter().map(|row| {
        let mut job:PublisherJob=serde_json::from_str(row.get("record")).map_err(storage)?;
        job.state=row.get("state"); job.detail=row.get("detail"); Ok(job)
    }).collect()
}
async fn job_result(store:&RuntimeStore,job:&PublisherJob,state:&str,detail:&str)->Result<()> {
    sqlx::query("UPDATE publisher_jobs SET state=?,detail=? WHERE job_id=? AND state IN ('queued','dispatching')")
        .bind(state).bind(detail).bind(&job.job_id).execute(&store.pool).await.map_err(storage)?;
    Ok(())
}
async fn attempts(store:&RuntimeStore,policy:&PolicyRecord)->Result<u64> {
    let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM publisher_jobs WHERE policy_id=? AND dispatched=1")
        .bind(&policy.policy_id).fetch_one(&store.pool).await.map_err(storage)?;
    Ok(count as u64)
}
async fn window(store:&RuntimeStore,policy:&PolicyRecord)->Result<ObservationWindow> {
    let encoded:Option<String>=sqlx::query_scalar("SELECT record FROM publisher_windows WHERE policy_id=? AND revision=?")
        .bind(&policy.policy_id).bind(policy.revision).fetch_optional(&store.pool).await.map_err(storage)?;
    encoded.map(|value|serde_json::from_str(&value).map_err(storage)).transpose().map(Option::unwrap_or_default)
}
async fn save_window(store:&RuntimeStore,policy:&PolicyRecord,value:&ObservationWindow)->Result<()> {
    sqlx::query("INSERT INTO publisher_windows VALUES (?,?,?) ON CONFLICT(policy_id) DO UPDATE SET revision=excluded.revision,record=excluded.record")
        .bind(&policy.policy_id).bind(policy.revision).bind(serde_json::to_string(value).map_err(storage)?)
        .execute(&store.pool).await.map_err(storage)?;
    Ok(())
}
fn require_role(user:&CurrentUser)->Result<()> {
    if !matches!(user.role,Role::Engineer|Role::Admin) {
        return Err(RuntimeError(StatusCode::FORBIDDEN,"Publication mutation requires an engineer or administrator and an explicit profile scope".into()));
    }
    Ok(())
}
async fn current_identity(state:&AppState,id:i64)->Result<(CurrentUser,i64)> {
    let current=state.auth.get_user_auth_state(id).await.map_err(|_|storage("Publication identity unavailable"))?
        .ok_or_else(||invalid("Publication identity no longer exists"))?;
    if current.disabled || current.force_password_change {return Err(invalid("Publication identity was revoked"));}
    let user=CurrentUser{id:current.id,username:current.username,role:current.role};
    require_role(&user)?;
    Ok((user,current.auth_version))
}
async fn mutation_identity(state:&AppState,user:&CurrentUser,profile_hash:&str,permission:&str)->Result<i64> {
    let (current,version)=current_identity(state,user.id).await?;
    if current.role!=user.role {return Err(invalid("Session role changed"));}
    state.runtime.profile(profile_hash,&current,permission)?;
    Ok(version)
}
async fn grant_identity(state:&AppState,policy:&PolicyRecord)->Result<CurrentUser> {
    let id=policy.grantor_id.ok_or_else(||invalid("No independent publication grant"))?;
    let (user,version)=current_identity(state,id).await?;
    let profile=state.runtime.profiles.get(&policy.spec.profile_hash).ok_or_else(||invalid("Commissioned publication scope was revoked"))?;
    policy.validate_grant(&user,version,profile,Utc::now())?;
    Ok(user)
}
impl PolicyRecord {
    pub(crate) fn validate_grant(&self,user:&CurrentUser,auth_version:i64,profile:&DeploymentProfile,now:DateTime<Utc>)->Result<()> {
        require_role(user)?;
        if self.grantor_id!=Some(user.id) || self.grantor_auth_version!=Some(auth_version)
            || user.id==self.spec_author_id || user.id==self.created_by
            || self.state!="enabled" || self.spec.expires_at<=now || profile.profile_hash!=self.spec.profile_hash
            || profile.target!=self.spec.target
            || [GRANT,"runtime.deploy","runtime.operate"].iter().any(|permission|!profile.permits(user,permission)) {
            return Err(invalid("Independent publication grant expired or authorizing role, identity or exact scope was revoked"));
        }
        Ok(())
    }
}

async fn bundle(state:&AppState,id:&str)->Result<FrozenVersionBundle> {
    if !valid_hash(id) {return Err(invalid("An exact immutable candidate SHA-256 is required"));}
    let bundle=state.research.version_bundle(id).await.map_err(|e|RuntimeError(e.0,e.1))?;
    bundle.verify(id).map_err(|e|RuntimeError(e.0,e.1))?;
    Ok(bundle)
}
fn manifest(bundle:&FrozenVersionBundle)->Result<Value> {
    serde_json::from_str(&bundle.manifest_json).map_err(|_|invalid("Invalid immutable manifest"))
}
fn verify_scope(profile:&DeploymentProfile,bundle:&FrozenVersionBundle,realm:&str)->Result<Value> {
    bundle.verify(&bundle.version_id).map_err(|e|RuntimeError(e.0,e.1))?;
    let manifest=manifest(bundle)?;
    let equipment:BTreeSet<String>=serde_json::from_value(manifest["equipment_instance_ids"].clone())
        .map_err(|_|invalid("Candidate has no exact commissioned equipment binding"))?;
    if manifest["deployment_profile_hash"]!=profile.profile_hash
        || manifest["target"]!=serde_json::to_value(&profile.target).map_err(storage)?
        || equipment!=profile.equipment_instance_ids || manifest["operating_envelope"]!=profile.operating_envelope
        || manifest["author_identity_realm"]!=realm
        || manifest["author_user_id"].as_i64().is_none_or(|id|id<=0) {
        return Err(invalid("Candidate scope or authenticated author identity realm differs from the commissioned target"));
    }
    if manifest["holdout"]["independent"].as_bool().is_none()
        || manifest["holdout"]["status"].as_str().is_none_or(str::is_empty)
        || (profile.commissioning_kind=="field" && manifest["holdout"]["independent"]!=true)
        || manifest["model_test"]["passed"]!=true || manifest["model_test"]["used_for_selection"]!=false
        || manifest["assessment"]["eligible"]!=true || manifest["assessment"]["reasons"]!=json!([]) {
        return Err(invalid("Independent held-out qualification is missing; synthetic evidence never grants field eligibility"));
    }
    let hashes=manifest["evidence_hashes"].as_object().ok_or_else(||invalid("Candidate lacks immutable qualification evidence hashes"))?;
    if hashes.is_empty() || hashes.values().any(|hash|hash.as_str().is_none_or(|value|!valid_hash(value))) {
        return Err(invalid("Qualification evidence hash set is incomplete"));
    }
    super::verify_candidate_model(profile,&bundle.model_json)?;
    Ok(manifest)
}
pub(crate) fn candidate_author(policy:&PolicyRecord,manifest:&Value,realm:&str)->Result<i64> {
    let author=manifest["author_user_id"].as_i64().ok_or_else(||invalid("Candidate author identity missing"))?;
    if manifest["author_identity_realm"]!=realm || author<=0 || Some(author)==policy.grantor_id {
        return Err(invalid("Candidate author cannot grant their own publication, and identity realms must match"));
    }
    if manifest["policy_id"]!=policy.spec.source_policy_id
        || manifest["policy_revision"]!=policy.spec.source_policy_revision
        || manifest["policy_definition_sha256"]!=policy.spec.source_policy_sha256
        || manifest["policy_origin_study_id"]!=policy.spec.source_study_id
        || manifest["baseline_version_id"]!=policy.spec.baseline_candidate_hash
        || manifest["baseline_model_sha256"]!=policy.spec.baseline_model_sha256 {
        return Err(invalid("Candidate research policy, study or baseline lineage differs from the independent grant"));
    }
    Ok(author)
}
async fn current_candidate(state:&AppState,policy:&PolicyRecord)->Result<FrozenVersionBundle> {
    let current=jobs(&state.runtime,Some(&policy.policy_id)).await?.into_iter()
        .find(|job|job.state=="accepted").map(|job|job.candidate_hash)
        .unwrap_or_else(||policy.spec.baseline_candidate_hash.clone());
    bundle(state,&current).await
}
async fn validate_candidate(state:&AppState,policy:&PolicyRecord,candidate:&FrozenVersionBundle,current:&FrozenVersionBundle)->Result<Value> {
    let profile=state.runtime.profiles.get(&policy.spec.profile_hash).ok_or_else(||invalid("Commissioned profile was removed"))?;
    let realm=identity_realm(&state.runtime).await?;
    let manifest=verify_scope(profile,candidate,&realm)?;
    candidate_author(policy,&manifest,&realm)?;
    let baseline=bundle(state,&policy.spec.baseline_candidate_hash).await?;
    if baseline.model_sha256!=policy.spec.baseline_model_sha256 {return Err(invalid("Policy baseline byte digest changed"));}
    verify_scope(profile,&baseline,&realm)?;
    verify_scope(profile,current,&realm)?;
    let before:Value=serde_json::from_str(&current.model_json).map_err(storage)?;
    let after:Value=serde_json::from_str(&candidate.model_json).map_err(storage)?;
    policy.spec.validate_transition(&before,&after)?;
    // The current version may evolve, but its non-tuning structure must still equal the original baseline.
    let baseline_model:Value=serde_json::from_str(&baseline.model_json).map_err(storage)?;
    let pointers=policy.spec.allowlist.iter().map(|bound|bound.pointer.as_str()).collect();
    if !unchanged_except(&baseline_model,&after,&pointers,&mut String::new()) {
        return Err(invalid("Candidate physics/tags/Ts/hard limits differ from the commissioned baseline"));
    }
    Ok(manifest)
}
pub(crate) fn validate_status_freshness(status:&Value,maximum_acquisition_age_ms:u64,now:DateTime<Utc>)->Result<()> {
    let maximum_solve_age_ms=status["maximum_solve_age_ms"].as_u64()
        .filter(|limit|*limit>0 && *limit<=i64::MAX as u64)
        .ok_or_else(||invalid("Native qualified-solve freshness budget is missing"))?;
    if status["forecast_issued_at"]!=status["observed_at"] {
        return Err(invalid("Native forecast is not bound to the last qualified solve"));
    }
    for (field,maximum_age_ms) in [("observed_at",maximum_solve_age_ms),("acquired_at",maximum_acquisition_age_ms)] {
        let sampled_at=status[field].as_str().and_then(|value|DateTime::parse_from_rfc3339(value).ok())
            .map(|time|time.with_timezone(&Utc)).ok_or_else(||invalid(format!("Native {field} timestamp is missing")))?;
        let age=now.signed_duration_since(sampled_at).num_milliseconds();
        if age<0 || age as u64>maximum_age_ms {
            return Err(invalid(format!("Native {field} is stale or future dated for its own freshness budget")));
        }
    }
    Ok(())
}
async fn observe(state:&AppState,policy:&PolicyRecord)->Result<PublicationObservation> {
    if state.settings.research.enabled || !state.runtime.runtime_enabled {return Err(invalid("Research deployment has no publication transport"));}
    let profile=state.runtime.profiles.get(&policy.spec.profile_hash).ok_or_else(||invalid("Commissioned profile was removed"))?;
    let status=super::transport::status(state,profile).await?;
    let now=Utc::now();
    validate_status_freshness(&status,profile.maximum_age_ms,now)?;
    let samples=state.runtime.observations.read().await;
    let mut first=None; let mut last=None;
    let mut values=BTreeMap::new();
    for tag in std::iter::once(&profile.acquisition_sequence_node).chain(policy.spec.operating_envelope.iter().map(|bound|&bound.tag)) {
        let sample=samples.get(tag).ok_or_else(||invalid("Publication envelope observation is missing"))?;
        let time=DateTime::parse_from_rfc3339(&sample.timestamp).map_err(|_|invalid("Producer source timestamp is missing"))?.with_timezone(&Utc);
        let age=now.signed_duration_since(time).num_milliseconds();
        let value=sample.value.as_f64().filter(|value|value.is_finite()).ok_or_else(||invalid("Publication observation is not finite numeric data"))?;
        if sample.status&0xc0000000!=0 || sample.source_endpoint!=profile.endpoint || age<0 || age>profile.maximum_age_ms as i64 {
            return Err(invalid("Publication requires fresh good-quality data from the exact commissioned source"));
        }
        first=Some(first.map_or(time,|previous:DateTime<Utc>|previous.min(time)));
        last=Some(last.map_or(time,|previous:DateTime<Utc>|previous.max(time)));
        values.insert(tag,value);
    }
    let sampled_at=first.ok_or_else(||invalid("Publication observation window is empty"))?;
    if last.ok_or_else(||invalid("Publication observation window is empty"))?.signed_duration_since(sampled_at).num_milliseconds()>profile.maximum_skew_ms as i64 {
        return Err(invalid("Publication envelope is not a coherent acquisition"));
    }
    for bound in &policy.spec.operating_envelope {
        let value=values[&bound.tag];
        if value<bound.minimum || value>bound.maximum {return Err(invalid(format!("Observed operating envelope exceeded: {}",bound.tag)));}
    }
    let sequence=values[&profile.acquisition_sequence_node];
    if sequence<0.0 || sequence>9_007_199_254_740_991.0 || sequence.fract()!=0.0 || sequence as u64%2!=0 {
        return Err(invalid("Publication requires a committed even acquisition sequence"));
    }
    drop(samples);
    let pending=state.runtime.pending_count(&profile.profile_hash).await?;
    let observed=PublicationObservation {
        instance:status["instance_id"].as_str().ok_or_else(||invalid("Native instance missing"))?.to_owned(),
        version:status["model_sha256"].as_str().ok_or_else(||invalid("Native version missing"))?.to_owned(),
        epoch:status["epoch"].as_i64().ok_or_else(||invalid("Native epoch missing"))?,
        sequence:sequence as u64, sampled_at,
        healthy:status["healthy"]==true && status["pending_application"]==false,
        armed:status["armed"]==true, engaged:status["mode"]=="engaged", unresolved_operations:pending>0,
    };
    if !observed.eligible() {return Err(invalid("Publication never starts, arms or engages equipment; existing healthy explicitly armed Engage is required"));}
    Ok(observed)
}
async fn queue_locked(state:&AppState,policy:&PolicyRecord,candidate_hash:&str,message:&str)->Result<PublisherJob> {
    grant_identity(state,policy).await?;
    if let Some(existing)=jobs(&state.runtime,Some(&policy.policy_id)).await?.into_iter().find(|job|job.candidate_hash==candidate_hash) {
        return Ok(existing);
    }
    let blocked:i64=sqlx::query_scalar("SELECT COUNT(*) FROM publisher_jobs WHERE policy_id=? AND state IN ('queued','dispatching','unknown')")
        .bind(&policy.policy_id).fetch_one(&state.runtime.pool).await.map_err(storage)?;
    if blocked>0 {return Err(invalid("A queued or ambiguous publication already blocks this policy"));}
    let candidate=bundle(state,candidate_hash).await?;
    let current=current_candidate(state,policy).await?;
    validate_candidate(state,policy,&candidate,&current).await?;
    let observed=observe(state,policy).await?;
    if observed.version!=current.model_sha256 {return Err(invalid("Current native version is outside the policy's accepted immutable chain"));}
    let mut observed_window=window(&state.runtime,policy).await?;
    observed_window.observe(observed.clone(),state.runtime.profiles[&policy.spec.profile_hash].maximum_age_ms);
    save_window(&state.runtime,policy,&observed_window).await?;
    if !observed_window.ready(&policy.spec,attempts(&state.runtime,policy).await?) {
        return Err(invalid("Actual good-data observation, cooldown or change budget has not qualified"));
    }
    let now=Utc::now();
    let job_id=digest(json!([policy.policy_id,policy.revision_sha256,candidate_hash,observed.instance,observed.version,observed.epoch]).to_string().as_bytes());
    let job=PublisherJob {
        operation_id:format!("publisher_{job_id}"), job_id, policy_id:policy.policy_id.clone(),
        policy_revision:policy.revision, policy_revision_sha256:policy.revision_sha256.clone(), profile_hash:policy.spec.profile_hash.clone(),
        candidate_hash:candidate.version_id, model_sha256:candidate.model_sha256, manifest_sha256:candidate.manifest_sha256,
        expected_candidate_hash:current.version_id, expected_instance:observed.instance, expected_version:observed.version,
        expected_epoch:observed.epoch, expires_at:policy.spec.expires_at.min(now+chrono::Duration::seconds(300)),
        state:"queued".into(), created_at:now, detail:message.into(),
    };
    enqueue(&state.runtime,&job).await?;
    record_decision(&state.runtime,policy,Some(candidate_hash),"queued","Frozen candidate, instance, version and epoch; bounded policy grant, not individual human approval").await?;
    Ok(job)
}
async fn enqueue(store:&RuntimeStore,job:&PublisherJob)->Result<()> {
    sqlx::query("INSERT INTO publisher_jobs(job_id,policy_id,candidate_hash,state,detail,record) VALUES (?,?,?,'queued',?,?)")
        .bind(&job.job_id).bind(&job.policy_id).bind(&job.candidate_hash).bind(&job.detail)
        .bind(serde_json::to_string(job).map_err(storage)?).execute(&store.pool).await.map_err(storage)?;
    Ok(())
}
async fn claim(store:&RuntimeStore,policy:&PolicyRecord,job:&PublisherJob,actor_id:i64,request:&OperationRequest)->Result<()> {
    if !store.runtime_enabled {return Err(invalid("Read-only research cannot claim a field publication"));}
    if !matches!(&request.action,OperationAction::SwitchVersion{candidate_hash,..} if candidate_hash==&job.candidate_hash)
        || request.operation_id!=job.operation_id || request.profile_hash!=job.profile_hash
        || request.expected_version!=job.expected_version || request.expected_epoch!=job.expected_epoch
        || request.expected_instance.as_deref()!=Some(job.expected_instance.as_str()) || request.expires_at!=job.expires_at
        || request.expires_at<=Utc::now() {
        return Err(invalid("Publication journal accepts only the exact frozen native SwitchVersion intent"));
    }
    let mut transaction=store.pool.begin().await.map_err(storage)?;
    let original:String=sqlx::query_scalar("SELECT record FROM publisher_jobs WHERE job_id=?")
        .bind(&job.job_id).fetch_optional(&mut *transaction).await.map_err(storage)?
        .ok_or_else(||invalid("Publication job was never registered"))?;
    if serde_json::from_str::<Value>(&original).map_err(storage)?!=serde_json::to_value(job).map_err(storage)? {
        return Err(invalid("Publication claim cannot change the durable frozen job"));
    }
    let head:Option<i64>=sqlx::query_scalar("SELECT revision FROM publisher_heads WHERE policy_id=?")
        .bind(&policy.policy_id).fetch_optional(&mut *transaction).await.map_err(storage)?;
    if head!=Some(job.policy_revision) || policy.revision!=job.policy_revision
        || policy.revision_sha256!=job.policy_revision_sha256 || policy.policy_id!=job.policy_id
        || policy.grantor_id!=Some(actor_id) || policy.state!="enabled" {
        return Err(invalid("Publication grant changed before atomic claim"));
    }
    let used:i64=sqlx::query_scalar("SELECT COUNT(*) FROM publisher_jobs WHERE policy_id=? AND dispatched=1")
        .bind(&policy.policy_id).fetch_one(&mut *transaction).await.map_err(storage)?;
    if used as u64>=policy.spec.max_changes {return Err(invalid("Publication total dispatch budget exhausted"));}
    let claimed=sqlx::query("UPDATE publisher_jobs SET state='dispatching',dispatched=1,detail='One-shot policy switch claim committed before native transport' WHERE job_id=? AND state='queued'")
        .bind(&job.job_id).execute(&mut *transaction).await.map_err(storage)?;
    if claimed.rows_affected()!=1 {return Err(invalid("Publication claim was already consumed; never retry"));}
    let now=Utc::now().to_rfc3339();
    sqlx::query("INSERT INTO runtime_operations(id,profile_hash,actor_id,state,request,detail,created_at,updated_at) VALUES (?,?,?,'executing',?,'Bounded policy grant; no individual candidate approval, start, arm or Engage',?,?)")
        .bind(&request.operation_id).bind(&request.profile_hash).bind(actor_id)
        .bind(serde_json::to_string(request).map_err(storage)?).bind(&now).bind(&now)
        .execute(&mut *transaction).await.map_err(storage)?;
    sqlx::query("DELETE FROM publisher_windows WHERE policy_id=?").bind(&policy.policy_id)
        .execute(&mut *transaction).await.map_err(storage)?;
    transaction.commit().await.map_err(storage)
}

async fn publisher_switch_locked(state:&AppState,policy:&PolicyRecord,job:&PublisherJob)->Result<()> {
    let actor=grant_identity(state,policy).await?;
    let candidate=bundle(state,&job.candidate_hash).await?;
    if candidate.model_sha256!=job.model_sha256 || candidate.manifest_sha256!=job.manifest_sha256 {
        return Err(invalid("Queued immutable candidate bytes changed"));
    }
    let current=bundle(state,&job.expected_candidate_hash).await?;
    if current.model_sha256!=job.expected_version {return Err(invalid("Queued current candidate no longer binds its model digest"));}
    let manifest=validate_candidate(state,policy,&candidate,&current).await?;
    let observed=observe(state,policy).await?;
    job.validate_execution(policy,&observed,Utc::now())?;
    let profile=state.runtime.profiles.get(&job.profile_hash).ok_or_else(||invalid("Commissioned profile was removed"))?;
    let mut observed_window=window(&state.runtime,policy).await?;
    observed_window.observe(observed,profile.maximum_age_ms);
    save_window(&state.runtime,policy,&observed_window).await?;
    if !observed_window.ready(&policy.spec,attempts(&state.runtime,policy).await?) {
        return Err(invalid("Publication observation, cooldown or total dispatch budget is no longer qualified"));
    }
    let realm=identity_realm(&state.runtime).await?;
    let approval_id=format!("policy_{}_{}",policy.policy_id,policy.revision);
    let request=OperationRequest {
        operation_id:job.operation_id.clone(),profile_hash:job.profile_hash.clone(),expected_version:job.expected_version.clone(),
        expected_epoch:job.expected_epoch,expires_at:job.expires_at,
        expected_instance:Some(job.expected_instance.clone()),
        reason:format!("Bounded policy {} revision {}; not individual human candidate approval",policy.policy_id,policy.revision),
        action:OperationAction::SwitchVersion{candidate_hash:job.candidate_hash.clone(),approval_id},
    };
    let approval=json!({
        "kind":"bounded_policy","profile_hash":profile.profile_hash,"candidate_hash":candidate.version_id,
        "model_sha256":candidate.model_sha256,"manifest_sha256":candidate.manifest_sha256,"target":profile.target,
        "equipment_instance_ids":profile.equipment_instance_ids,"operating_envelope":profile.operating_envelope,
        "author_id":manifest["author_user_id"],"approver_id":actor.id,"approver_auth_version":policy.grantor_auth_version,
        "author_identity_realm":realm,"approver_identity_realm":realm,"expires_at":job.expires_at,
        "policy_id":policy.policy_id,"policy_revision":policy.revision,"policy_revision_sha256":policy.revision_sha256,
        "source_policy_id":policy.spec.source_policy_id,"source_policy_revision":policy.spec.source_policy_revision,
        "source_policy_sha256":policy.spec.source_policy_sha256,"source_study_id":policy.spec.source_study_id,
        "individual_candidate_approval":false,"version_bundle":candidate,
    });
    // Revalidate grant immediately before the atomic one-shot claim. All policy changes share dispatch.
    grant_identity(state,policy).await?;
    if request.expires_at<=Utc::now() {return Err(invalid("Frozen publication intent expired before dispatch"));}
    claim(&state.runtime,policy,job,actor.id,&request).await?;
    // The only native mutation in this module. Neither expiry nor transport errors requeue it.
    let (outcome,detail)=match super::transport::dispatch(state,profile,&request,Some(approval)).await {
        Ok(response) if response["ok"]==true => ("accepted",response.to_string()),
        Ok(response) => ("rejected",response.to_string()),
        Err(error) => ("unknown",error.1),
    };
    state.runtime.finish(&request.operation_id,outcome,&detail).await?;
    job_result(&state.runtime,job,outcome,&detail).await?;
    record_decision(&state.runtime,policy,Some(&job.candidate_hash),outcome,&detail).await?;
    state.auth.audit(Some(&actor),"runtime.publisher.switch",Some(&job.job_id),outcome,
        Some("Explicit independently granted bounded policy; not individual human candidate approval")).await;
    state.runtime.event(&job.profile_hash,&job.operation_id,if outcome=="accepted"{"info"}else{"warning"},&detail).await?;
    Ok(())
}

pub(crate) fn routes()->Router<AppState> {
    Router::new()
        .route("/api/runtime/publisher",get(snapshot))
        .route("/api/runtime/publisher/policies",post(create))
        .route("/api/runtime/publisher/policies/:id",get(history))
        .route("/api/runtime/publisher/policies/:id/revise",post(revise))
        .route("/api/runtime/publisher/policies/:id/state",post(change_state))
        .route("/api/runtime/publisher/policies/:id/queue",post(queue))
}
fn require_runtime(state:&AppState)->Result<()> {
    if state.settings.research.enabled || !state.runtime.runtime_enabled {return Err(invalid("Isolated research cannot mutate field publication policy"));}
    Ok(())
}
async fn create(State(state):State<AppState>,user:CurrentUser,_:CsrfVerified,Json(body):Json<CreatePolicy>)->Result<Json<PolicyRecord>> {
    require_runtime(&state)?; reason(&body.reason)?;
    if body.expected_revision!=0 || body.policy_id.is_empty() || body.policy_id.len()>64
        || !body.policy_id.bytes().all(|b|b.is_ascii_alphanumeric() || b==b'_' || b==b'-') {
        return Err(invalid("New policy requires a safe unique ID and expected_revision=0"));
    }
    let _guard=state.runtime.dispatch.lock().await;
    mutation_identity(&state,&user,&body.spec.profile_hash,MANAGE).await?;
    let profile=state.runtime.profile(&body.spec.profile_hash,&user,MANAGE)?;
    let baseline=bundle(&state,&body.spec.baseline_candidate_hash).await?;
    verify_scope(profile,&baseline,&identity_realm(&state.runtime).await?)?;
    body.spec.validate(profile,&baseline)?;
    let record=PolicyRecord {
        policy_id:body.policy_id,revision:1,revision_sha256:String::new(),state:"paused".into(),spec:body.spec,
        created_by:user.id,spec_author_id:user.id,revision_by:user.id,grantor_id:None,grantor_auth_version:None,
        created_at:Utc::now(),reason:body.reason,
    };
    let record=save_revision(&state.runtime,record,0).await?;
    record_decision(&state.runtime,&record,None,"created_paused",&record.reason).await?;
    Ok(Json(record))
}
async fn revise(State(state):State<AppState>,user:CurrentUser,_:CsrfVerified,Path(id):Path<String>,Json(body):Json<RevisePolicy>)->Result<Json<PolicyRecord>> {
    require_runtime(&state)?; reason(&body.reason)?;
    let _guard=state.runtime.dispatch.lock().await;
    let mut record=latest(&state.runtime,&id).await?;
    mutation_identity(&state,&user,&record.spec.profile_hash,MANAGE).await?;
    if record.state=="revoked" || record.revision!=body.expected_revision || record.spec.profile_hash!=body.spec.profile_hash {
        return Err(invalid("Revoked policies, stale revisions and moving a policy to another physical profile are forbidden"));
    }
    let baseline=bundle(&state,&body.spec.baseline_candidate_hash).await?;
    let profile=state.runtime.profile(&body.spec.profile_hash,&user,MANAGE)?;
    verify_scope(profile,&baseline,&identity_realm(&state.runtime).await?)?;
    body.spec.validate(profile,&baseline)?;
    if body.spec.baseline_candidate_hash!=record.spec.baseline_candidate_hash || body.spec.baseline_model_sha256!=record.spec.baseline_model_sha256 {
        return Err(invalid("A different commissioned baseline requires a new independently granted policy"));
    }
    record.spec=body.spec; record.spec_author_id=user.id; record.revision_by=user.id; record.reason=body.reason;
    record.state="paused".into(); record.grantor_id=None; record.grantor_auth_version=None;
    let record=save_revision(&state.runtime,record,body.expected_revision).await?;
    record_decision(&state.runtime,&record,None,"revised_paused",&record.reason).await?;
    Ok(Json(record))
}
async fn change_state(State(state):State<AppState>,user:CurrentUser,_:CsrfVerified,Path(id):Path<String>,Json(body):Json<ChangeState>)->Result<Json<PolicyRecord>> {
    require_runtime(&state)?; reason(&body.reason)?;
    if !matches!(body.state.as_str(),"enabled"|"paused"|"revoked") {return Err(invalid("State must be enabled, paused or revoked"));}
    let _guard=state.runtime.dispatch.lock().await;
    let mut record=latest(&state.runtime,&id).await?;
    let permission=if body.state=="enabled" || state.runtime.profile(&record.spec.profile_hash,&user,GRANT).is_ok(){GRANT}else{MANAGE};
    let version=mutation_identity(&state,&user,&record.spec.profile_hash,permission).await?;
    if record.state=="revoked" || record.revision!=body.expected_revision {return Err(invalid("Revoked policy is terminal or revision changed"));}
    if body.state=="enabled" {
        let profile=state.runtime.profile(&record.spec.profile_hash,&user,GRANT)?;
        for permission in ["runtime.deploy","runtime.operate"] {state.runtime.profile(&record.spec.profile_hash,&user,permission)?;}
        let baseline=bundle(&state,&record.spec.baseline_candidate_hash).await?;
        let manifest=verify_scope(profile,&baseline,&identity_realm(&state.runtime).await?)?;
        record.spec.validate(profile,&baseline)?;
        if user.id==record.created_by || user.id==record.spec_author_id || manifest["author_user_id"]==user.id {
            return Err(invalid("Policy specification and candidate authors cannot grant their own automatic publication"));
        }
        let current=current_candidate(&state,&record).await?;
        let status=super::transport::status(&state,profile).await?;
        if status["model_sha256"]!=current.model_sha256 {return Err(invalid("Current native baseline differs from the immutable granted chain"));}
        record.grantor_id=Some(user.id); record.grantor_auth_version=Some(version);
    } else {
        record.grantor_id=None; record.grantor_auth_version=None;
    }
    record.state=body.state; record.reason=body.reason; record.revision_by=user.id;
    let record=save_revision(&state.runtime,record,body.expected_revision).await?;
    record_decision(&state.runtime,&record,None,&record.state,&record.reason).await?;
    state.auth.audit(Some(&user),"runtime.publisher.policy",Some(&record.policy_id),&record.state,
        Some("Policy grant only; no individual candidate human review asserted")).await;
    Ok(Json(record))
}
async fn queue(State(state):State<AppState>,user:CurrentUser,_:CsrfVerified,Path(id):Path<String>,Json(body):Json<QueueCandidate>)->Result<Json<PublisherJob>> {
    require_runtime(&state)?; reason(&body.reason)?;
    let _guard=state.runtime.dispatch.lock().await;
    let record=latest(&state.runtime,&id).await?;
    mutation_identity(&state,&user,&record.spec.profile_hash,MANAGE).await?;
    if record.revision!=body.expected_revision {return Err(invalid("Publication policy revision changed"));}
    queue_locked(&state,&record,&body.candidate_hash,&body.reason).await.map(Json)
}
async fn history(State(state):State<AppState>,user:CurrentUser,Path(id):Path<String>)->Result<Json<Value>> {
    let record=latest(&state.runtime,&id).await?;
    state.runtime.profile(&record.spec.profile_hash,&user,"runtime.view")?;
    let rows:Vec<String>=sqlx::query_scalar("SELECT record FROM publisher_revisions WHERE policy_id=? ORDER BY revision DESC LIMIT 501")
        .bind(&id).fetch_all(&state.runtime.pool).await.map_err(storage)?;
    let truncated=rows.len()>500;
    let revisions:Vec<PolicyRecord>=rows.into_iter().take(500).map(|value|serde_json::from_str(&value).map_err(storage)).collect::<Result<_>>()?;
    Ok(Json(json!({"policy":record,"revisions":revisions,"truncated":truncated})))
}

fn tuning_catalogue(model:&Value)->Vec<Value> {
    let mut values=Vec::new();
    for (group,fields) in [("cvs",&["weight","alpha","slack_weight"][..]),("mvs",&["weight_r","target_weight"][..])] {
        if let Some(variables)=model["variables"][group].as_array() {
            for (index,variable) in variables.iter().enumerate() {
                for field in fields {
                    if let Some(value)=variable[*field].as_f64().filter(|value|value.is_finite()) {
                        values.push(json!({"pointer":format!("/variables/{group}/{index}/{field}"),
                            "label":format!("{} / {field}",variable["name"].as_str().unwrap_or(group)),"value":value}));
                    }
                }
            }
        }
    }
    for field in ["prediction_horizon","control_horizon"] {
        if let Some(value)=model["tuning"][field].as_u64() {
            values.push(json!({"pointer":format!("/tuning/{field}"),"label":field,"value":value}));
        }
    }
    values
}
async fn snapshot(State(state):State<AppState>,user:CurrentUser)->Result<Json<Value>> {
    // This read performs artifact reads only: no observation accrual, claims, reconciliation or dispatch.
    let records:Vec<_>=policies(&state.runtime).await?.into_iter()
        .filter(|policy|state.runtime.profile(&policy.spec.profile_hash,&user,"runtime.view").is_ok()).collect();
    let visible:BTreeSet<_>=records.iter().map(|policy|policy.policy_id.as_str()).collect();
    let all_jobs=jobs(&state.runtime,None).await?;
    let jobs_truncated=all_jobs.len()>500;
    let shown_jobs:Vec<_>=all_jobs.into_iter().take(500).filter(|job|visible.contains(job.policy_id.as_str())).collect();
    let rows=sqlx::query("SELECT * FROM publisher_decisions ORDER BY id DESC LIMIT 501")
        .fetch_all(&state.runtime.pool).await.map_err(storage)?;
    let decisions_truncated=rows.len()>500;
    let decisions:Vec<_>=rows.into_iter().take(500).filter_map(|row| {
        let policy_id:String=row.get("policy_id");
        visible.contains(policy_id.as_str()).then(||PublisherDecision {
            id:row.get("id"),policy_id,revision:row.get("revision"),candidate_hash:row.get("candidate_hash"),
            outcome:row.get("outcome"),reason:row.get("reason"),created_at:row.get("created_at"),
        })
    }).collect();
    let mut progress=Vec::new();
    for record in &records {
        let window=window(&state.runtime,record).await?;
        let used=attempts(&state.runtime,record).await?;
        progress.push(json!({"policy_id":record.policy_id,"revision":record.revision,"good_samples":window.good_samples,
            "good_observation_ms":window.good_ms,"changes_reserved":used,"budget_remaining":record.spec.max_changes.saturating_sub(used),
            "grant_current":grant_identity(&state,record).await.is_ok(),"last_observation":window.last}));
    }
    let mut sources=Vec::new();
    let source_status=match state.research.publisher_policies().await {
        Ok(rows)=>{
            let realm=identity_realm(&state.runtime).await?;
            for mut row in rows.into_iter().take(64) {
                let Some(template)=row["publication_template_version_id"].as_str() else {continue;};
                let Ok(artifact)=bundle(&state,template).await else {continue;};
                let Ok(meta)=manifest(&artifact) else {continue;};
                let Some(profile_hash)=meta["deployment_profile_hash"].as_str() else {continue;};
                let Ok(profile)=state.runtime.profile(profile_hash,&user,"runtime.view") else {continue;};
                if verify_scope(profile,&artifact,&realm).is_err() {continue;}
                let model:Value=serde_json::from_str(&artifact.model_json).map_err(storage)?;
                row["profile_hash"]=json!(profile_hash);
                row["baseline_model_sha256"]=json!(artifact.model_sha256);
                row["tuning_parameters"]=json!(tuning_catalogue(&model));
                sources.push(row);
            }
            json!({"status":"available"})
        }
        Err(error)=>json!({"status":"unavailable","reason":error.1}),
    };
    let row=sqlx::query("SELECT observed_at,status FROM publisher_scheduler WHERE id=1")
        .fetch_one(&state.runtime.pool).await.map_err(storage)?;
    let observed_at:Option<String>=row.get("observed_at");
    let fresh=observed_at.as_deref().and_then(|value|DateTime::parse_from_rfc3339(value).ok())
        .is_some_and(|time|(0..=30).contains(&Utc::now().signed_duration_since(time).num_seconds()));
    Ok(Json(json!({"policies":records,"jobs":shown_jobs,"decisions":decisions,"progress":progress,
        "source_policies":sources,"source_policy_status":source_status,
        "scheduler":{"status":row.get::<String,_>("status"),"observed_at":observed_at,"fresh":fresh,"interval_seconds":polling_interval(&state.runtime).as_secs_f64()},
        "jobs_truncated":jobs_truncated,"decisions_truncated":decisions_truncated,
        "automatic_start":false,"automatic_arm":false,"automatic_engage":false,
        "authority_kind":"independent_bounded_policy_not_individual_candidate_approval"})))
}

fn polling_interval(store:&RuntimeStore)->std::time::Duration {
    // Leave headroom for producer sample boundaries and scheduler jitter without relaxing source gaps.
    store.profiles.values().fold(std::time::Duration::from_secs(POLL_SECONDS),|period,profile|
        period.min(std::time::Duration::from_millis(profile.maximum_age_ms)/2))
}

/// Main calls this once after assembling AppState. There is no enabled-by-default policy.
pub(crate) fn spawn_scheduler(state:AppState) {
    if state.settings.research.enabled || !state.runtime.runtime_enabled {return;}
    tokio::spawn(async move {
        let period=polling_interval(&state.runtime);
        let discovery_ticks=std::time::Duration::from_secs(30).as_nanos().div_ceil(period.as_nanos()) as u64;
        let mut interval=tokio::time::interval(period);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut tick=0u64;
        loop {
            interval.tick().await;
            let result=scheduler_tick(&state,tick%discovery_ticks==0).await;
            let status=match result {Ok(())=>"running",Err(error)=>{tracing::error!("Publication scheduler: {}",error.1);"blocked"}};
            if let Err(error)=sqlx::query("UPDATE publisher_scheduler SET observed_at=?,status=? WHERE id=1")
                .bind(Utc::now().to_rfc3339()).bind(status).execute(&state.runtime.pool).await {
                tracing::error!("Publication scheduler heartbeat is not durable: {error}");
            }
            tick=tick.wrapping_add(1);
        }
    });
}
async fn scheduler_tick(state:&AppState,discover:bool)->Result<()> {
    for observed_policy in policies(&state.runtime).await? {
        let _guard=state.runtime.dispatch.lock().await;
        let policy=latest(&state.runtime,&observed_policy.policy_id).await?;
        if policy.state!="enabled" {continue;}
        // Only consume already recorded original-operation outcomes. Never resend or infer an effect.
        for job in jobs(&state.runtime,Some(&policy.policy_id)).await? {
            if job.state=="unknown" || job.state=="dispatching" {
                if let Some(operation)=state.runtime.operation(&job.operation_id).await? {
                    if let Some(outcome)=job.reconciled_state(&operation) {
                        let detail=if outcome=="cleared_unknown" {
                            json!({"original_state":"unknown","original_detail":operation.detail,
                                "clearance_id":operation.clearance_id,"cleared_at":operation.cleared_at,
                                "clearance_evidence":operation.clearance_evidence,
                                "automatic_retry":false,"candidate_application_inferred":false}).to_string()
                        }else{operation.detail};
                        sqlx::query("UPDATE publisher_jobs SET state=?,detail=? WHERE job_id=? AND state IN ('unknown','dispatching')")
                            .bind(outcome).bind(&detail).bind(&job.job_id)
                            .execute(&state.runtime.pool).await.map_err(storage)?;
                        save_window(&state.runtime,&policy,&ObservationWindow::default()).await?;
                        record_decision(&state.runtime,&policy,Some(&job.candidate_hash),outcome,
                            if outcome=="cleared_unknown"{"Explicit maintenance baseline clearance; original effect remains unknown, candidate never retried and no accepted baseline inferred"}
                            else{"Original durable operation outcome reconciled without transport replay"}).await?;
                    }
                }
            }
        }
        let pending=jobs(&state.runtime,Some(&policy.policy_id)).await?;
        if let Some(job)=pending.iter().find(|job|job.state=="queued") {
            if let Err(error)=publisher_switch_locked(state,&policy,job).await {
                // A storage error after a claim is never misreported as pre-dispatch rejection.
                sqlx::query("UPDATE publisher_jobs SET state='rejected',detail=? WHERE job_id=? AND state='queued'")
                    .bind(&error.1).bind(&job.job_id).execute(&state.runtime.pool).await.map_err(storage)?;
                record_decision(&state.runtime,&policy,Some(&job.candidate_hash),"blocked",&error.1).await?;
            }
            continue;
        }
        if pending.iter().any(|job|matches!(job.state.as_str(),"unknown"|"dispatching")) {
            record_decision(&state.runtime,&policy,None,"blocked","Ambiguous publication effect remains queryable; no retry or new automatic switch").await?;
            continue;
        }
        let observed=match grant_identity(state,&policy).await {
            Ok(_)=>observe(state,&policy).await,
            Err(error)=>Err(error),
        };
        let observed=match observed {
            Ok(observed)=>observed,
            Err(error)=>{
                save_window(&state.runtime,&policy,&ObservationWindow::default()).await?;
                record_decision(&state.runtime,&policy,None,"blocked",&error.1).await?;
                continue;
            }
        };
        let expected=pending.iter().find(|job|job.state=="accepted").map(|job|job.model_sha256.as_str())
            .unwrap_or(&policy.spec.baseline_model_sha256);
        if observed.version!=expected {
            save_window(&state.runtime,&policy,&ObservationWindow::default()).await?;
            record_decision(&state.runtime,&policy,None,"blocked","Native version left the accepted immutable policy chain").await?;
            continue;
        }
        let mut window=window(&state.runtime,&policy).await?;
        window.observe(observed,state.runtime.profiles[&policy.spec.profile_hash].maximum_age_ms);
        save_window(&state.runtime,&policy,&window).await?;
        if !window.ready(&policy.spec,attempts(&state.runtime,&policy).await?) {
            record_decision(&state.runtime,&policy,None,"observing","Waiting for actual good-data observation/cooldown within the total change budget").await?;
            continue;
        }
        if !discover {continue;}
        let candidates=match state.research.publisher_candidates(&policy.spec.source_policy_id,&policy.spec.source_study_id).await {
            Ok(candidates)=>candidates,
            Err(error)=>{record_decision(&state.runtime,&policy,None,"discovery_unavailable",&error.1).await?;continue;}
        };
        for candidate in candidates.into_iter().take(128) {
            if pending.iter().any(|job|job.candidate_hash==candidate) || candidate==policy.spec.baseline_candidate_hash {continue;}
            match queue_locked(state,&policy,&candidate,"Automatic discovery of server-owned qualified research artifact; no human candidate review asserted").await {
                Ok(_)=>break,
                Err(error)=>record_decision(&state.runtime,&policy,Some(&candidate),"candidate_rejected",&error.1).await?,
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> PolicySpec {
        PolicySpec {
            profile_hash: "a".repeat(64), target: Target { plant:"p".into(), line:"l".into(), area:"a".into(), group:"g".into() },
            baseline_candidate_hash:"b".repeat(64), baseline_model_sha256:"c".repeat(64),
            source_policy_id:"research_1".into(), source_study_id:"study_1".into(),
            source_policy_revision:1, source_policy_sha256:"9".repeat(64),
            allowlist:vec![NumericBound { pointer:"/variables/cvs/0/weight".into(), minimum:1.0, maximum:10.0, max_delta:2.0 }],
            operating_envelope:vec![EnvelopeBound { tag:"feed:PV".into(), minimum:10.0, maximum:100.0 }],
            minimum_observation_ms:10_000, minimum_good_samples:3, cooldown_ms:20_000, max_changes:2,
            expires_at:Utc::now()+chrono::Duration::hours(1),
        }
    }
    fn model(weight: f64) -> Value {
        json!({"metadata":{"name":"mill"},"tuning":{"sample_time":1},"physics":{"gain":[[2.0]]},
            "variables":{"cvs":[{"weight":weight,"node_ids":{"pv":"feed:PV"},"limits":{"low_low":0,"high_high":100}}]}})
    }
    fn observation(seconds: i64, sequence: u64) -> PublicationObservation {
        PublicationObservation { instance:"instance_1".into(), version:"c".repeat(64), epoch:7, sequence,
            sampled_at:DateTime::from_timestamp(1_700_000_000+seconds,0).unwrap(),
            healthy:true, armed:true, engaged:true, unresolved_operations:false }
    }
    fn policy() -> PolicyRecord {
        PolicyRecord { policy_id:"publisher_1".into(), revision:2, revision_sha256:"d".repeat(64), state:"enabled".into(),
            spec:spec(), created_by:1, spec_author_id:1, revision_by:2, grantor_id:Some(2), grantor_auth_version:Some(4),
            created_at:Utc::now(), reason:"independent bounded grant".into() }
    }
    fn job() -> PublisherJob {
        PublisherJob { job_id:"job_1".into(), policy_id:"publisher_1".into(), policy_revision:2,
            policy_revision_sha256:"d".repeat(64), profile_hash:"a".repeat(64), candidate_hash:"e".repeat(64),
            model_sha256:"f".repeat(64), manifest_sha256:"1".repeat(64), expected_candidate_hash:"b".repeat(64),
            expected_instance:"instance_1".into(), expected_version:"c".repeat(64), expected_epoch:7,
            expires_at:Utc::now()+chrono::Duration::seconds(60), state:"queued".into(), operation_id:"publisher_job_1".into(),
            created_at:Utc::now(), detail:"frozen".into() }
    }
    fn profile() -> DeploymentProfile {
        serde_json::from_value(json!({
            "profile_hash":"a".repeat(64),"target":{"plant":"p","line":"l","area":"a","group":"g"},
            "equipment_instance_ids":["mill"],"engine_id":"mill","model_sha256":"c".repeat(64),
            "endpoint":"opc.tcp://127.0.0.1:4840","model_file":"mill.json","model_binding":{},
            "forecast_tag":"forecast","observation_tag":"observation","variables":[],
            "grants":[{"user_id":2,"permissions":[GRANT,"runtime.deploy","runtime.operate"]}],
            "approved_by":"commissioner","commissioning_evidence":"test fixture","commissioning_kind":"synthetic_local",
            "operating_envelope":"commissioned","maximum_age_ms":10_000,"maximum_skew_ms":100,"application_tolerance":0.001,
            "handoff":{"release_tag":"release","release_value":0.0,"acknowledgement_tag":"ack",
                "acknowledgement_value":0.0,"timeout_ms":1000,"watchdog_evidence":"fixture",
                "acquire_value":1.0,"watchdog_tag":"watchdog","watchdog_timeout_ms":1000,"local_mode":0.0},
            "acquisition_sequence_node":"sequence","history_capacity_samples":10
        })).unwrap()
    }
    fn switch_request(job:&PublisherJob)->OperationRequest {
        OperationRequest { operation_id:job.operation_id.clone(),profile_hash:job.profile_hash.clone(),
            expected_version:job.expected_version.clone(),expected_epoch:job.expected_epoch,
            expected_instance:Some(job.expected_instance.clone()),expires_at:job.expires_at,reason:"bounded publication".into(),
            action:OperationAction::SwitchVersion{candidate_hash:job.candidate_hash.clone(),approval_id:"bounded-policy".into()} }
    }

    #[test]
    fn transition_rejects_out_of_bounds_and_every_uncommissioned_model_change() {
        let spec=spec(); let old=model(4.0);
        assert!(spec.validate_transition(&old,&model(5.0)).is_ok());
        assert!(spec.validate_transition(&old,&model(7.0)).is_err());
        assert!(spec.validate_transition(&model(9.0),&model(11.0)).is_err());
        for pointer in ["/physics/gain/0/0","/tuning/sample_time","/variables/cvs/0/limits/high_high"] {
            let mut changed=model(5.0); *changed.pointer_mut(pointer).unwrap()=json!(99);
            assert!(spec.validate_transition(&old,&changed).is_err(),"{pointer}");
        }
        let mut changed=model(5.0); changed["variables"]["cvs"][0]["node_ids"]["pv"]=json!("other:PV");
        assert!(spec.validate_transition(&old,&changed).is_err());
        let mut wide=spec.clone(); wide.allowlist[0].pointer="/physics/gain/0/0".into();
        assert!(wide.validate_transition(&old,&model(5.0)).is_err());
    }

    #[test]
    fn frozen_jobs_reject_epoch_authority_change_and_never_auto_arm() {
        let policy=policy(); let job=job(); let healthy=observation(20,8);
        assert!(job.validate_execution(&policy,&healthy,Utc::now()).is_ok());
        let mut changed=healthy.clone(); changed.epoch+=1;
        assert!(job.validate_execution(&policy,&changed,Utc::now()).is_err());
        changed=healthy.clone(); changed.instance="restarted".into();
        assert!(job.validate_execution(&policy,&changed,Utc::now()).is_err());
        changed=healthy.clone(); changed.armed=false;
        assert!(job.validate_execution(&policy,&changed,Utc::now()).is_err());
        changed=healthy.clone(); changed.engaged=false;
        assert!(job.validate_execution(&policy,&changed,Utc::now()).is_err());
        changed=healthy.clone(); changed.unresolved_operations=true;
        assert!(job.validate_execution(&policy,&changed,Utc::now()).is_err());
        let mut revoked=policy.clone(); revoked.state="revoked".into();
        assert!(job.validate_execution(&revoked,&healthy,Utc::now()).is_err());
        assert!(job.validate_execution(&policy,&healthy,job.expires_at).is_err());
    }

    #[test]
    fn observation_and_cooldown_require_distinct_continuous_good_source_data() {
        let policy=spec(); let mut window=ObservationWindow::default();
        window.observe(observation(0,2),10_000);
        for _ in 0..100 { window.observe(observation(0,2),10_000); }
        assert!(!window.ready(&policy,0));
        window.observe(observation(10,4),10_000);
        window.observe(observation(20,6),10_000);
        assert!(window.ready(&policy,0));
        assert!(!window.ready(&policy,2));
        let mut bad=observation(25,8); bad.healthy=false;
        window.observe(bad,10_000);
        assert!(!window.ready(&policy,0));
        window.observe(observation(100,10),10_000);
        assert!(!window.ready(&policy,0));
    }

    #[tokio::test]
    async fn polling_preserves_qualified_source_continuity_across_sample_boundaries() {
        let mut store=RuntimeStore::open("sqlite::memory:",vec![],true).await.unwrap();
        let mut commissioned=profile(); commissioned.maximum_age_ms=5_000;
        store.profiles.insert(commissioned.profile_hash.clone(),commissioned);
        let period=polling_interval(&store).as_millis() as u64;
        let mut window=ObservationWindow::default();
        // A one-second producer straddles the poll boundary with two milliseconds of jitter.
        for tick in 0..=10 {
            let sampled_seconds=(999+tick*period+if tick%2==1 {2}else{0})/1_000;
            window.observe(observation(sampled_seconds as i64,2+2*sampled_seconds),5_000);
        }
        assert!(window.ready(&spec(),0),"Healthy source samples must qualify at the actual scheduler cadence");
        assert!(!window.ready(&spec(),spec().max_changes));
        let last=window.last.as_ref().unwrap();
        let gap=PublicationObservation { sequence:last.sequence+12,
            sampled_at:last.sampled_at+chrono::Duration::seconds(6), ..last.clone() };
        window.observe(gap,5_000);
        assert!(!window.ready(&spec(),0),"A real source gap must still invalidate qualification");
    }

    #[test]
    fn independent_grant_rejects_author_self_grant_role_downgrade_and_revoked_scope() {
        let policy=policy(); let mut profile=profile();
        let mut grantor=CurrentUser{id:2,username:"independent".into(),role:Role::Engineer};
        assert!(policy.validate_grant(&grantor,4,&profile,Utc::now()).is_ok());
        assert!(policy.validate_grant(&grantor,5,&profile,Utc::now()).is_err());
        grantor.role=Role::Viewer;
        assert!(policy.validate_grant(&grantor,4,&profile,Utc::now()).is_err());
        grantor.role=Role::Engineer;
        profile.grants[0].permissions.remove(GRANT);
        assert!(policy.validate_grant(&grantor,4,&profile,Utc::now()).is_err());
        let mut self_granted=policy.clone(); self_granted.spec_author_id=2;
        assert!(self_granted.validate_grant(&grantor,4,&super::tests::profile(),Utc::now()).is_err());
        let mut meta=json!({"author_user_id":1,"author_identity_realm":"realm-a",
            "policy_id":"research_1","policy_revision":1,"policy_definition_sha256":"9".repeat(64),
            "policy_origin_study_id":"study_1","baseline_version_id":"b".repeat(64),"baseline_model_sha256":"c".repeat(64)});
        assert_eq!(candidate_author(&policy,&meta,"realm-a").unwrap(),1);
        assert!(candidate_author(&policy,&meta,"unrelated-auth-database").is_err());
        meta["author_user_id"]=json!(2);
        assert!(candidate_author(&policy,&meta,"realm-a").is_err());
        meta["author_user_id"]=json!(1); meta["policy_revision"]=json!(2);
        assert!(candidate_author(&policy,&meta,"realm-a").is_err());
    }

    #[tokio::test]
    async fn immutable_revision_compare_and_swap_revokes_queued_work() {
        let store=RuntimeStore::open("sqlite::memory:",vec![],true).await.unwrap();
        let original=save_revision(&store,policy(),0).await.unwrap();
        let mut queued=job(); queued.policy_revision=original.revision; queued.policy_revision_sha256=original.revision_sha256.clone();
        enqueue(&store,&queued).await.unwrap();
        let mut paused=original.clone(); paused.state="paused".into(); paused.grantor_id=None;
        let revision=save_revision(&store,paused,original.revision).await.unwrap();
        assert_eq!(revision.revision,2);
        assert_ne!(original.revision_sha256,revision.revision_sha256);
        assert!(save_revision(&store,original.clone(),original.revision).await.is_err());
        assert_eq!(latest(&store,&original.policy_id).await.unwrap().state,"paused");
        assert_eq!(jobs(&store,None).await.unwrap()[0].state,"rejected");
        assert!(claim(&store,&original,&queued,2,&switch_request(&queued)).await.is_err());
        assert!(store.operation(&queued.operation_id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn uncertain_native_transport_consumes_claim_and_budget_without_retry() {
        let store=RuntimeStore::open("sqlite::memory:",vec![],true).await.unwrap();
        let mut definition=policy(); definition.spec.max_changes=1;
        let policy=save_revision(&store,definition,0).await.unwrap();
        let mut queued=job(); queued.policy_revision=policy.revision; queued.policy_revision_sha256=policy.revision_sha256.clone();
        enqueue(&store,&queued).await.unwrap();
        let request=switch_request(&queued);
        claim(&store,&policy,&queued,2,&request).await.unwrap();
        store.finish(&request.operation_id,"unknown","Native transport ended without an acknowledgement").await.unwrap();
        job_result(&store,&queued,"unknown","Native transport ended without an acknowledgement").await.unwrap();
        assert!(claim(&store,&policy,&queued,2,&request).await.is_err());
        assert!(enqueue(&store,&queued).await.is_err());
        assert_eq!(jobs(&store,None).await.unwrap()[0].state,"unknown");
        assert_eq!(store.operation(&request.operation_id).await.unwrap().unwrap().state,"unknown");
        assert_eq!(attempts(&store,&policy).await.unwrap(),1);
        let mut another=queued.clone(); another.job_id="another".into(); another.operation_id="another".into(); another.candidate_hash="8".repeat(64);
        enqueue(&store,&another).await.unwrap();
        assert!(claim(&store,&policy,&another,2,&switch_request(&another)).await.is_err());
        assert!(store.operation("another").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn startup_recovery_rejects_frozen_queue_and_preserves_ambiguous_claim_without_native_action() {
        let mut store=RuntimeStore::open("sqlite::memory:",vec![],true).await.unwrap();
        store.profiles.insert("a".repeat(64),profile());
        let policy=save_revision(&store,policy(),0).await.unwrap();
        let realm=identity_realm(&store).await.unwrap();
        let mut claimed=job(); claimed.policy_revision=policy.revision; claimed.policy_revision_sha256=policy.revision_sha256.clone();
        enqueue(&store,&claimed).await.unwrap();
        claim(&store,&policy,&claimed,2,&switch_request(&claimed)).await.unwrap();
        let mut waiting=claimed.clone(); waiting.job_id="waiting".into(); waiting.operation_id="waiting".into(); waiting.candidate_hash="8".repeat(64);
        enqueue(&store,&waiting).await.unwrap();
        let mut observed=ObservationWindow::default(); observed.observe(observation(0,2),10_000);
        save_window(&store,&policy,&observed).await.unwrap();
        store.runtime_enabled=false;
        init(&store).await.unwrap();
        assert_eq!(jobs(&store,None).await.unwrap().iter().find(|j|j.job_id==claimed.job_id).unwrap().state,"dispatching");
        assert_eq!(jobs(&store,None).await.unwrap().iter().find(|j|j.job_id==waiting.job_id).unwrap().state,"queued");
        store.runtime_enabled=true;
        recover(&store).await.unwrap();
        assert_eq!(identity_realm(&store).await.unwrap(),realm);
        let records=jobs(&store,None).await.unwrap();
        assert_eq!(records.iter().find(|j|j.job_id==claimed.job_id).unwrap().state,"unknown");
        assert_eq!(records.iter().find(|j|j.job_id==waiting.job_id).unwrap().state,"rejected");
        assert_eq!(window(&store,&policy).await.unwrap().good_samples,0);
        assert!(claim(&store,&policy,&claimed,2,&switch_request(&claimed)).await.is_err());
        assert!(claim(&store,&policy,&waiting,2,&switch_request(&waiting)).await.is_err());
        assert!(store.operation("waiting").await.unwrap().is_none());
        let other=RuntimeStore::open("sqlite::memory:",vec![],false).await.unwrap();
        assert_ne!(identity_realm(&other).await.unwrap(),realm);
    }

    #[tokio::test]
    async fn publication_journal_cannot_convert_switch_into_start_arm_or_engage() {
        let store=RuntimeStore::open("sqlite::memory:",vec![],true).await.unwrap();
        let policy=save_revision(&store,policy(),0).await.unwrap();
        let mut queued=job(); queued.policy_revision=policy.revision; queued.policy_revision_sha256=policy.revision_sha256.clone();
        enqueue(&store,&queued).await.unwrap();
        for action in [OperationAction::Start{},OperationAction::Monitor{},OperationAction::Arm{},OperationAction::Engage{}] {
            let mut request=switch_request(&queued); request.action=action;
            assert!(claim(&store,&policy,&queued,2,&request).await.is_err());
        }
        let mut refreshed=queued.clone(); refreshed.expected_epoch+=1;
        assert!(claim(&store,&policy,&refreshed,2,&switch_request(&refreshed)).await.is_err());
        assert!(store.operation(&queued.operation_id).await.unwrap().is_none());
        assert_eq!(jobs(&store,None).await.unwrap()[0].state,"queued");
    }

    #[tokio::test]
    async fn explicit_clearance_keeps_unknown_effect_and_cannot_become_candidate_acceptance() {
        let store=RuntimeStore::open("sqlite::memory:",vec![],true).await.unwrap();
        let policy=save_revision(&store,policy(),0).await.unwrap();
        let mut queued=job(); queued.policy_revision=policy.revision; queued.policy_revision_sha256=policy.revision_sha256.clone();
        enqueue(&store,&queued).await.unwrap();
        claim(&store,&policy,&queued,2,&switch_request(&queued)).await.unwrap();
        store.finish(&queued.operation_id,"unknown","Unacknowledged native switch").await.unwrap();
        let mut original=store.operation(&queued.operation_id).await.unwrap().unwrap();
        assert_eq!(queued.reconciled_state(&original),None);
        original.blocking=false;
        assert_eq!(queued.reconciled_state(&original),None);
        original.clearance_id=Some("independent-maintenance-clearance".into());
        original.cleared_at=Some(Utc::now().to_rfc3339());
        original.clearance_evidence=Some(json!({"safe_baseline":true,"epoch":8}));
        assert_eq!(queued.reconciled_state(&original),Some("cleared_unknown"));
        assert_eq!(original.state,"unknown");
        original.operation_id="unrelated".into();
        assert_eq!(queued.reconciled_state(&original),None);
        assert!(claim(&store,&policy,&queued,2,&switch_request(&queued)).await.is_err());
        assert!(enqueue(&store,&queued).await.is_err());
    }

    #[test]
    fn exported_mv_weight_r_is_catalogued_and_granted_without_a_weight_alias() {
        let before=json!({"variables":{"mvs":[{"name":"feed","weight_r":4.0,"target_weight":0.0}]}});
        let mut after=before.clone(); after["variables"]["mvs"][0]["weight_r"]=json!(5.0);
        let mut grant=spec();
        grant.allowlist=vec![NumericBound{pointer:"/variables/mvs/0/weight_r".into(),minimum:1.0,maximum:10.0,max_delta:2.0}];
        let catalogue=tuning_catalogue(&before);
        assert!(catalogue.iter().any(|entry|entry["pointer"]=="/variables/mvs/0/weight_r" && entry["value"]==4.0));
        assert!(!catalogue.iter().any(|entry|entry["pointer"]=="/variables/mvs/0/weight"));
        assert!(grant.validate_transition(&before,&after).is_ok());
        after["variables"]["mvs"][0]["weight_r"]=json!(7.0);
        assert!(grant.validate_transition(&before,&after).is_err());
        grant.allowlist[0].pointer="/variables/mvs/0/weight".into();
        assert!(grant.validate_transition(&before,&after).is_err());
    }

    #[test]
    fn sixty_second_solve_and_one_second_acquisition_have_distinct_freshness_budgets() {
        let now=DateTime::from_timestamp(1_700_000_100,0).unwrap();
        let solved=now-chrono::Duration::seconds(50);
        let mut status=json!({"observed_at":solved,"forecast_issued_at":solved,
            "acquired_at":now-chrono::Duration::seconds(1),"maximum_solve_age_ms":75_000});
        assert!(validate_status_freshness(&status,5_000,now).is_ok());
        status["acquired_at"]=json!(now-chrono::Duration::seconds(6));
        assert!(validate_status_freshness(&status,5_000,now).is_err());
        status["acquired_at"]=json!(now-chrono::Duration::seconds(1));
        status["observed_at"]=json!(now-chrono::Duration::seconds(76));
        status["forecast_issued_at"]=status["observed_at"].clone();
        assert!(validate_status_freshness(&status,5_000,now).is_err());
        status["observed_at"]=json!(solved); status["forecast_issued_at"]=json!(now);
        assert!(validate_status_freshness(&status,5_000,now).is_err());
        status["forecast_issued_at"]=json!(solved); status["maximum_solve_age_ms"]=Value::Null;
        assert!(validate_status_freshness(&status,5_000,now).is_err());
    }
}
