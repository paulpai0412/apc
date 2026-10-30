//! Source-health condition history and independently evidenced interlock clearance.
use super::*;

pub(super) async fn init(store:&RuntimeStore)->Result<()> {
    for sql in [
        "CREATE TABLE IF NOT EXISTS runtime_condition_events (event_id INTEGER PRIMARY KEY,profile_hash TEXT NOT NULL,condition_key TEXT NOT NULL,active INTEGER NOT NULL,cleared_at TEXT,last_evaluated TEXT NOT NULL)",
        "CREATE UNIQUE INDEX IF NOT EXISTS runtime_active_condition ON runtime_condition_events(profile_hash,condition_key) WHERE active=1",
        "CREATE TABLE IF NOT EXISTS runtime_clearances (id TEXT PRIMARY KEY,profile_hash TEXT NOT NULL,actor_id INTEGER NOT NULL,request TEXT NOT NULL,evidence TEXT NOT NULL,created_at TEXT NOT NULL)",
        "CREATE TABLE IF NOT EXISTS runtime_operation_clearances (operation_id TEXT PRIMARY KEY,clearance_id TEXT NOT NULL)",
    ] {sqlx::query(sql).execute(&store.pool).await.map_err(storage)?;}
    Ok(())
}

pub(super) async fn condition(store:&RuntimeStore,profile:&str,key:&str,active:bool,detail:&str)->Result<()> {
    let now=Utc::now().to_rfc3339();let mut tx=store.pool.begin().await.map_err(storage)?;
    let existing:Option<i64>=sqlx::query_scalar("SELECT event_id FROM runtime_condition_events WHERE profile_hash=? AND condition_key=? AND active=1")
        .bind(profile).bind(key).fetch_optional(&mut *tx).await.map_err(storage)?;
    match (existing,active) {
        (None,true)=>{
            let row=sqlx::query("INSERT INTO runtime_events(profile_hash,source,severity,condition,first_seen,last_seen) VALUES (?,?,'warning',?,?,?)")
                .bind(profile).bind(key).bind(detail).bind(&now).bind(&now).execute(&mut *tx).await.map_err(storage)?;
            sqlx::query("INSERT INTO runtime_condition_events VALUES (?,?,?,1,NULL,?)")
                .bind(row.last_insert_rowid()).bind(profile).bind(key).bind(&now).execute(&mut *tx).await.map_err(storage)?;
        },
        (Some(id),true)=>{
            sqlx::query("UPDATE runtime_events SET last_seen=? WHERE id=?").bind(&now).bind(id).execute(&mut *tx).await.map_err(storage)?;
            sqlx::query("UPDATE runtime_condition_events SET last_evaluated=? WHERE event_id=?").bind(&now).bind(id).execute(&mut *tx).await.map_err(storage)?;
        },
        (Some(id),false)=>{
            sqlx::query("UPDATE runtime_condition_events SET active=0,cleared_at=?,last_evaluated=? WHERE event_id=?")
                .bind(&now).bind(&now).bind(id).execute(&mut *tx).await.map_err(storage)?;
        },(None,false)=>{},
    }
    tx.commit().await.map_err(storage)?;Ok(())
}

pub(super) async fn tick(store:&RuntimeStore)->Result<()> {
    if !store.runtime_enabled{return Ok(());}
    let now=Utc::now();let observations=store.observations.read().await;let mut conditions=Vec::new();
    for profile in store.profiles.values() {
        let mut missing=false;let mut bad=false;let mut stale=false;let mut first=None;let mut last=None;
        let tags=profile.variables.iter().flat_map(|v|std::iter::once(v.pv_tag.as_str()).chain(
            [v.applied_tag.as_deref(),v.op_tag.as_deref(),v.mode_tag.as_deref()].into_iter().flatten()))
            .chain(std::iter::once(profile.acquisition_sequence_node.as_str()));
        for tag in tags {
            let Some(sample)=observations.get(tag).filter(|sample|sample.source_endpoint==profile.endpoint)else{missing=true;continue;};
            bad|=sample.status&0xc0000000!=0;
            let Ok(time)=DateTime::parse_from_rfc3339(&sample.timestamp).map(|t|t.with_timezone(&Utc))else{stale=true;continue;};
            let age=now.signed_duration_since(time).num_milliseconds();
            stale|=age>profile.maximum_age_ms as i64||age < -(profile.maximum_skew_ms as i64);
            first=Some(first.map_or(time,|value:DateTime<Utc>|value.min(time)));
            last=Some(last.map_or(time,|value:DateTime<Utc>|value.max(time)));
        }
        let skew=first.zip(last).is_some_and(|(first,last)|(last-first).num_milliseconds()>profile.maximum_skew_ms as i64);
        conditions.push((profile.profile_hash.as_str(),missing,bad,stale,skew));
    }
    drop(observations);
    for (profile,missing,bad,stale,skew) in conditions {
        for (key,active,detail) in [
            ("runtime.acquisition_missing",missing,"Commissioned acquisition observations unavailable; not a DCS alarm"),
            ("runtime.acquisition_bad_quality",bad,"Commissioned source reports non-Good quality; not a DCS alarm"),
            ("runtime.acquisition_stale",stale,"Commissioned source time is stale, invalid or future; not a DCS alarm"),
            ("runtime.acquisition_skew",skew,"Commissioned acquisition exceeds source-time coherence bound; not a DCS alarm"),
        ] {condition(store,profile,key,active,detail).await?;}
    }
    Ok(())
}

pub(super) fn safe_native(profile:&DeploymentProfile,status:&Value)->Result<()> {
    if status["profile_hash"]!=profile.profile_hash||status["armed"]!=false||status["pending_application"]!=false
        ||status["safe_baseline"]!=true||status["writer_owner"]!=true
        ||status["dcs_handoff"]!=true||!matches!(status["mode"].as_str(),Some("monitor"|"hold"|"stopped")) {
        return Err(invalid("Native authority has not independently confirmed cessation and DCS handoff"));
    }
    let acquired=DateTime::parse_from_rfc3339(status["acquired_at"].as_str().ok_or_else(||invalid("Native acquisition time unavailable"))?)
        .map_err(|_|invalid("Invalid native acquisition clock"))?.with_timezone(&Utc);
    let age=Utc::now().signed_duration_since(acquired).num_milliseconds();
    if age<0||age>profile.maximum_age_ms as i64||status["instance_id"].as_str().is_none_or(str::is_empty)
        ||status["acquisition_sequence"].as_u64().is_none() {return Err(invalid("Safe baseline requires fresh independently acquired native evidence"));}
    Ok(())
}

async fn identity_version(state:&AppState,user:&CurrentUser)->Result<i64> {
    let current=state.auth.get_user_auth_state(user.id).await.map_err(|_|invalid("Maintenance identity unavailable"))?
        .ok_or_else(||invalid("Maintenance identity no longer exists"))?;
    if current.disabled||current.force_password_change||current.role!=user.role{return Err(RuntimeError(StatusCode::FORBIDDEN,"Identity was revoked during authorization".into()));}
    Ok(current.auth_version)
}

pub(crate) async fn maintenance<'a>(state:&'a AppState,user:&CurrentUser)->Result<tokio::sync::MutexGuard<'a,()>> {
    if user.role!=crate::auth::Role::Admin||state.settings.research.enabled{return Err(RuntimeError(StatusCode::FORBIDDEN,"Field maintenance requires an administrator on the runtime listener".into()));}
    let guard=state.runtime.dispatch.lock().await;
    let actor_version=identity_version(state,user).await?;let mut evidence=Vec::with_capacity(state.runtime.profiles.len());
    for profile in state.runtime.profiles.values() {
        state.runtime.profile(&profile.profile_hash,user,"runtime.maintenance")?;
        if state.runtime.pending_count(&profile.profile_hash).await? > 0{return Err(invalid("Unresolved operation blocks maintenance; independently clear its interlock first"));}
        let status=transport::status(state,profile).await?;safe_native(profile,&status)?;evidence.push((profile,status));
    }
    if identity_version(state,user).await?!=actor_version{return Err(invalid("Maintenance authorization version changed"));}
    for (profile,status) in &evidence {safe_native(profile,status)?;}
    state.auth.audit(Some(user),"runtime.maintenance.authorize",None,"accepted",Some("All configured commissioned groups independently safe; this does not authorize control")).await;
    Ok(guard)
}

#[derive(Deserialize,Serialize)]
#[serde(deny_unknown_fields)]
struct ClearanceRequest {
    clearance_id:String,profile_hash:String,operation_ids:BTreeSet<String>,expected_instance:String,
    expected_epoch:i64,expected_version:String,expires_at:DateTime<Utc>,reason:String,
}
pub(super) fn routes()->Router<AppState>{Router::new().route("/api/runtime/clearances",post(clear))}
async fn clear(State(state):State<AppState>,user:CurrentUser,_:CsrfVerified,Json(request):Json<ClearanceRequest>)->Result<Json<Value>> {
    if state.settings.research.enabled{return Err(invalid("Research cannot clear field operation interlocks"));}
    let _guard=state.runtime.dispatch.lock().await;
    let profile=state.runtime.profile(&request.profile_hash,&user,"runtime.operate")?;
    if request.clearance_id.is_empty()||request.clearance_id.len()>128||!request.clearance_id.bytes().all(|b|b.is_ascii_alphanumeric()||matches!(b,b'-'|b'_'))
        ||request.operation_ids.is_empty()||request.operation_ids.len()>64||request.reason.trim().is_empty()||request.reason.len()>1024
        ||request.expires_at<=Utc::now()||request.expires_at>Utc::now()+chrono::Duration::seconds(300){return Err(invalid("Invalid clearance identity, reason or expiry"));}
    let encoded=serde_json::to_string(&request).map_err(storage)?;
    if let Some(row)=sqlx::query("SELECT actor_id,request,evidence FROM runtime_clearances WHERE id=?").bind(&request.clearance_id).fetch_optional(&state.runtime.pool).await.map_err(storage)? {
        if row.get::<i64,_>("actor_id")!=user.id||row.get::<String,_>("request")!=encoded{return Err(invalid("Clearance ID already binds another request"));}
        return Ok(Json(json!({"clearance_id":request.clearance_id,"outcome_changed":false,"evidence":serde_json::from_str::<Value>(row.get("evidence")).map_err(storage)?})));
    }
    let actor_version=identity_version(&state,&user).await?;
    let status=transport::status(&state,profile).await?;safe_native(profile,&status)?;
    if status["instance_id"]!=request.expected_instance||status["epoch"].as_i64()!=Some(request.expected_epoch)||status["model_sha256"]!=request.expected_version {
        return Err(invalid("Native safe-baseline identity changed"));
    }
    for id in &request.operation_ids {
        let original=state.runtime.operation(id).await?.ok_or_else(||invalid("Unknown operation identity"))?;
        if original.profile_hash!=request.profile_hash||original.state!="unknown"||!original.blocking{return Err(invalid("Only blocking unresolved historical operations in this scope may be cleared"));}
        if original.request["expected_instance"]==request.expected_instance&&original.request["expected_epoch"].as_i64()==Some(request.expected_epoch) {
            return Err(invalid("Perform explicit Hold/Exit first: current epoch must invalidate delayed original commands"));
        }
    }
    if identity_version(&state,&user).await?!=actor_version||request.expires_at<=Utc::now(){return Err(invalid("Clearance authorization changed or expired"));}
    safe_native(profile,&status)?;
    let now=Utc::now().to_rfc3339();let mut tx=state.runtime.pool.begin().await.map_err(storage)?;
    sqlx::query("INSERT INTO runtime_clearances VALUES (?,?,?,?,?,?)").bind(&request.clearance_id).bind(&request.profile_hash).bind(user.id)
        .bind(encoded).bind(status.to_string()).bind(&now).execute(&mut *tx).await.map_err(storage)?;
    for id in &request.operation_ids {
        let inserted=sqlx::query("INSERT INTO runtime_operation_clearances(operation_id,clearance_id) SELECT id,? FROM runtime_operations WHERE id=? AND state='unknown' AND NOT EXISTS (SELECT 1 FROM runtime_operation_clearances c WHERE c.operation_id=runtime_operations.id)")
            .bind(&request.clearance_id).bind(id).execute(&mut *tx).await.map_err(storage)?;
        if inserted.rows_affected()!=1{return Err(invalid("Original outcome changed while clearance was being verified; refresh its exact receipt"));}
    }
    tx.commit().await.map_err(storage)?;
    state.runtime.event(&request.profile_hash,&request.clearance_id,"warning","Unknown historical outcomes retained; new-operation interlock cleared using independently confirmed current safe baseline").await?;
    state.auth.audit(Some(&user),"runtime.interlock.clear",Some(&request.profile_hash),"accepted",Some(&request.reason)).await;
    Ok(Json(json!({"clearance_id":request.clearance_id,"operation_ids":request.operation_ids,"outcome_changed":false,"evidence":status})))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile()->DeploymentProfile {
        serde_json::from_value(json!({
            "profile_hash":"fixture","target":{"plant":"test","line":"1","area":"cement","group":"mill"},
            "equipment_instance_ids":["fixture"],"engine_id":"fixture","model_sha256":"a".repeat(64),
            "endpoint":"opc.tcp://127.0.0.1:4840","model_file":"fixture.json","model_binding":null,
            "forecast_tag":"forecast","observation_tag":"observation","variables":[],
            "grants":[{"user_id":1,"permissions":["runtime.view","runtime.operate","runtime.deploy","runtime.maintenance"]}],
            "approved_by":"independent-fixture-reviewer","commissioning_evidence":"synthetic only","commissioning_kind":"synthetic_local",
            "operating_envelope":"fixture","maximum_age_ms":1000,"maximum_skew_ms":100,"application_tolerance":0.001,
            "handoff":{"release_tag":"release","release_value":1.0,"acknowledgement_tag":"ack","acknowledgement_value":1.0,
                "timeout_ms":1000,"watchdog_evidence":"fixture","acquire_value":0.0,"watchdog_tag":"watchdog","watchdog_timeout_ms":1000,"local_mode":0.0},
            "acquisition_sequence_node":"sequence","history_capacity_samples":100
        })).unwrap()
    }

    #[test]
    fn safe_baseline_requires_current_independent_native_attestation() {
        let profile=profile();
        let valid=json!({"profile_hash":"fixture","instance_id":"new-instance","acquisition_sequence":2,
            "acquired_at":Utc::now()-chrono::Duration::milliseconds(1),"mode":"monitor","armed":false,
            "pending_application":false,"dcs_handoff":true,"writer_owner":true,"safe_baseline":true});
        assert!(safe_native(&profile,&valid).is_ok());
        for (field,value) in [("safe_baseline",json!(false)),("writer_owner",json!(false)),("armed",json!(true)),
            ("pending_application",json!(true)),("dcs_handoff",json!(false)),("mode",json!("engaged")),
            ("acquired_at",json!("2000-01-01T00:00:00Z")),("acquisition_sequence",Value::Null),("instance_id",json!(""))] {
            let mut invalid=valid.clone();invalid[field]=value;
            assert!(safe_native(&profile,&invalid).is_err(),"accepted invalid {field}");
        }
    }

    #[test]
    fn scoped_grant_never_overrides_base_role_or_grants_another_user() {
        let profile=profile();
        for (role,operate,deploy,maintenance) in [
            (crate::auth::Role::Viewer,false,false,false),(crate::auth::Role::Operator,true,false,false),
            (crate::auth::Role::Engineer,true,true,false),(crate::auth::Role::Admin,true,true,true)] {
            let user=CurrentUser{id:1,username:"fixture".into(),role};
            assert!(profile.permits(&user,"runtime.view"));
            assert_eq!(profile.permits(&user,"runtime.operate"),operate);
            assert_eq!(profile.permits(&user,"runtime.deploy"),deploy);
            assert_eq!(profile.permits(&user,"runtime.maintenance"),maintenance);
        }
        assert!(!profile.permits(&CurrentUser{id:2,username:"ungranted".into(),role:crate::auth::Role::Admin},"runtime.operate"));
    }

    #[tokio::test]
    async fn condition_clear_and_recurrence_do_not_reuse_read_acknowledgement() {
        let store=RuntimeStore::open("sqlite::memory:",vec![],false).await.unwrap();
        condition(&store,"fixture","runtime.acquisition_stale",true,"stale source").await.unwrap();
        let first:i64=sqlx::query_scalar("SELECT event_id FROM runtime_condition_events WHERE active=1").fetch_one(&store.pool).await.unwrap();
        sqlx::query("INSERT INTO runtime_event_reads VALUES (?,1,'read','observed')").bind(first).execute(&store.pool).await.unwrap();
        condition(&store,"fixture","runtime.acquisition_stale",true,"stale source").await.unwrap();
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM runtime_events").fetch_one(&store.pool).await.unwrap(),1);
        condition(&store,"fixture","runtime.acquisition_stale",false,"source recovered").await.unwrap();
        let cleared=sqlx::query("SELECT active,cleared_at FROM runtime_condition_events WHERE event_id=?").bind(first).fetch_one(&store.pool).await.unwrap();
        assert_eq!(cleared.get::<i64,_>("active"),0);
        assert!(cleared.get::<Option<String>,_>("cleared_at").is_some());
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM runtime_event_reads").fetch_one(&store.pool).await.unwrap(),1);
        condition(&store,"fixture","runtime.acquisition_stale",true,"stale again").await.unwrap();
        let next:i64=sqlx::query_scalar("SELECT event_id FROM runtime_condition_events WHERE active=1").fetch_one(&store.pool).await.unwrap();
        assert_ne!(first,next);
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM runtime_event_reads WHERE event_id=?").bind(next).fetch_one(&store.pool).await.unwrap(),0);
    }

    #[tokio::test]
    async fn clearance_removes_only_interlock_not_unknown_historical_outcome() {
        let store=RuntimeStore::open("sqlite::memory:",vec![],false).await.unwrap();
        sqlx::query("INSERT INTO runtime_operations VALUES ('uncertain','fixture',1,'unknown','{}','no receipt','created','updated')").execute(&store.pool).await.unwrap();
        assert_eq!(store.pending_count("fixture").await.unwrap(),1);
        sqlx::query("INSERT INTO runtime_clearances VALUES ('clear','fixture',1,'{}','{\"safe_baseline\":true}','cleared')").execute(&store.pool).await.unwrap();
        sqlx::query("INSERT INTO runtime_operation_clearances VALUES ('uncertain','clear')").execute(&store.pool).await.unwrap();
        let operation=store.operation("uncertain").await.unwrap().unwrap();
        assert_eq!(operation.state,"unknown");assert!(!operation.blocking);
        assert_eq!(operation.clearance_id.as_deref(),Some("clear"));
        assert_eq!(store.pending_count("fixture").await.unwrap(),0);
        store.finish("uncertain","accepted","late callback").await.unwrap();
        store.reconcile_result("uncertain","accepted","late native receipt after concurrent clearance").await.unwrap();
        store.reconcile_result("uncertain","rejected","late installation receipt after clearance").await.unwrap();
        assert_eq!(store.operation("uncertain").await.unwrap().unwrap().state,"unknown");
    }
}
