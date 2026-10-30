//! Read-only resource observations. No directory traversal, retention deletion or external capacity guesses.
use super::*;
use std::{path::{Path as FsPath,PathBuf},time::Duration};
use futures::{stream::FuturesUnordered,StreamExt};

static FILESYSTEM_OBSERVERS:tokio::sync::Semaphore=tokio::sync::Semaphore::const_new(1);

pub(super) fn routes()->Router<AppState>{Router::new().route("/api/runtime/maintenance",get(overview))}

fn authorize(state:&AppState,user:&CurrentUser)->Result<()> {
    if state.settings.research.enabled||user.role!=crate::auth::Role::Admin||state.runtime.profiles.is_empty() {
        return Err(RuntimeError(StatusCode::FORBIDDEN,"Runtime maintenance overview requires an administrator and commissioned maintenance scope".into()));
    }
    for profile in state.runtime.profiles.values(){state.runtime.profile(&profile.profile_hash,user,"runtime.maintenance")?;}
    Ok(())
}

#[cfg(unix)]
fn filesystem(path:&FsPath)->Value {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path)=std::ffi::CString::new(path.as_os_str().as_bytes())else{return json!({"status":"unknown","total_bytes":null,"available_bytes":null,"reason":"Invalid local path"});};
    let mut stats=std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // The C path is NUL-terminated and the output buffer is read only after successful initialization.
    if unsafe{libc::statvfs(path.as_ptr(),stats.as_mut_ptr())}!=0 {
        return json!({"status":"unknown","total_bytes":null,"available_bytes":null,"reason":std::io::Error::last_os_error().to_string()});
    }
    let stats=unsafe{stats.assume_init()};let fragment=stats.f_frsize as u64;
    if fragment==0{return json!({"status":"unknown","total_bytes":null,"available_bytes":null,"reason":"Filesystem fragment size unavailable"});}
    json!({"status":"observed","total_bytes":(stats.f_blocks as u64).checked_mul(fragment),
        "available_bytes":(stats.f_bavail as u64).checked_mul(fragment),"scope":"filesystem_not_directory_quota","reason":null})
}
#[cfg(not(unix))]
fn filesystem(_path:&FsPath)->Value{json!({"status":"unknown","total_bytes":null,"available_bytes":null,"scope":"filesystem_not_directory_quota","reason":"Filesystem capacity observation is unavailable on this platform"})}

fn path_observation(name:&str,path:&str)->Value {
    if path.is_empty(){return json!({"name":name,"configured_path":null,"kind":"not_configured","file_bytes":null,"modified_at":null,"filesystem":{"status":"unknown","total_bytes":null,"available_bytes":null,"reason":"No configured local path"}});}
    let path=PathBuf::from(path);
    match std::fs::metadata(&path) {
        Ok(metadata)=>json!({"name":name,"configured_path":path,"kind":if metadata.is_file(){"file"}else if metadata.is_dir(){"directory"}else{"other"},
            "file_bytes":metadata.is_file().then_some(metadata.len()),"modified_at":metadata.modified().ok().map(DateTime::<Utc>::from),
            "filesystem":filesystem(&path)}),
        Err(error)=>json!({"name":name,"configured_path":path,"kind":if error.kind()==std::io::ErrorKind::NotFound{"absent"}else{"unknown"},
            "file_bytes":null,"modified_at":null,"filesystem":{"status":"unknown","total_bytes":null,"available_bytes":null,"reason":error.to_string()}}),
    }
}

async fn database(store:&RuntimeStore)->Value {
    let result:std::result::Result<Value,sqlx::Error>=async {
        let mut connection=store.pool.acquire().await?;
        let page_count:i64=sqlx::query_scalar("PRAGMA page_count").fetch_one(&mut *connection).await?;
        let page_size:i64=sqlx::query_scalar("PRAGMA page_size").fetch_one(&mut *connection).await?;
        let freelist:i64=sqlx::query_scalar("PRAGMA freelist_count").fetch_one(&mut *connection).await?;
        let journal:String=sqlx::query_scalar("PRAGMA journal_mode").fetch_one(&mut *connection).await?;
        let files=sqlx::query("PRAGMA database_list").fetch_all(&mut *connection).await?;
        let main_file=files.iter().find(|row|row.get::<String,_>("name")=="main").map(|row|row.get::<String,_>("file")).filter(|path|!path.is_empty());
        Ok(json!({"status":"observed","page_count":page_count,"page_size":page_size,"freelist_pages":freelist,
            "allocated_bytes":page_count.checked_mul(page_size),"free_page_bytes":freelist.checked_mul(page_size),
            "journal_mode":journal,"main_file":main_file,"error":null}))
    }.await;
    result.unwrap_or_else(|error|json!({"status":"unknown","page_count":null,"page_size":null,"freelist_pages":null,
        "allocated_bytes":null,"free_page_bytes":null,"journal_mode":null,"main_file":null,"error":error.to_string()}))
}

async fn configured_endpoint(state:&AppState)->(String,&'static str) {
    let stored=sqlx::query_scalar::<_,String>("SELECT value FROM infra_settings WHERE key='infrastructure_config'")
        .fetch_optional(&state.runtime.pool).await;
    if let Ok(Some(encoded))=stored {
        if let Ok(mut config)=serde_json::from_str::<crate::infrastructure::InfrastructureConfig>(&encoded) {
            // Match the existing worker's normalize_defaults selection, without creating or updating the registry.
            if config.opc_servers.is_empty(){config.opc_servers=crate::infrastructure::InfrastructureConfig::default().opc_servers;}
            if let Some(endpoint)=config.opc_servers.first().and_then(|server|server.opc_endpoint.as_ref()).filter(|endpoint|!endpoint.is_empty()) {
                return (endpoint.clone(),"infrastructure_registry");
            }
        }
    }
    (state.settings.opcua.endpoint_url.clone(),"settings_worker_fallback")
}

async fn controller_status<'a>(state:&AppState,profile:&'a DeploymentProfile)->(&'a DeploymentProfile,Option<Value>,Vec<String>) {
    let mut blockers=Vec::new();
    match state.runtime.pending_count(&profile.profile_hash).await {Ok(0)=>{},Ok(_)=>blockers.push("Unresolved operation interlock".into()),Err(error)=>blockers.push(error.1)}
    let native=match transport::status(state,profile).await {Ok(status)=>Some(status),Err(error)=>{blockers.push(error.1);None}};
    if let Some(status)=&native {if let Err(error)=safety::safe_native(profile,status){blockers.push(error.1);}}
    (profile,native,blockers)
}

async fn overview(State(state):State<AppState>,user:CurrentUser)->Result<Json<Value>> {
    authorize(&state,&user)?;
    let actor=state.auth.get_user_auth_state(user.id).await.map_err(|_|invalid("Maintenance identity unavailable"))?
        .ok_or_else(||invalid("Maintenance identity no longer exists"))?;
    let mut blockers=Vec::new();
    if !state.settings.runtime.connect_opcua{blockers.push("OPC worker disabled by configuration".to_owned());}
    if state.cmd_tx.is_closed(){blockers.push("OPC worker command channel is closed".to_owned());}
    if state.cmd_tx.capacity()==0{blockers.push("OPC worker command channel is full".to_owned());}
    if state.runtime.dispatch.try_lock().is_err(){blockers.push("Another operation or maintenance authorization is active".to_owned());}
    let statuses=async {
        let mut profiles=state.runtime.profiles.values();
        let mut pending=FuturesUnordered::new();
        for profile in profiles.by_ref().take(4){pending.push(controller_status(&state,profile));}
        let mut results=Vec::with_capacity(state.runtime.profiles.len());
        while let Some(status)=pending.next().await {
            results.push(status);
            if let Some(profile)=profiles.next(){pending.push(controller_status(&state,profile));}
        }
        results
    };
    let mut controllers=Vec::new();
    match tokio::time::timeout(Duration::from_secs(6),statuses).await {
        Ok(statuses)=>for (profile,native,mut local) in statuses {
            if let Some(status)=&native {if let Err(error)=safety::safe_native(profile,status){if !local.contains(&error.1){local.push(error.1);}}}
            blockers.extend(local.iter().map(|reason|format!("{}: {reason}",profile.engine_id)));
            controllers.push(json!({"profile_hash":profile.profile_hash,"engine_id":profile.engine_id,"endpoint":profile.endpoint,"native_status":native,"blockers":local}));
        },
        Err(_)=>{
            blockers.push("Controller safety observation deadline exceeded".into());
            for profile in state.runtime.profiles.values(){controllers.push(json!({"profile_hash":profile.profile_hash,"engine_id":profile.engine_id,"endpoint":profile.endpoint,"native_status":null,"blockers":["Controller safety observation deadline exceeded"]}));}
        }
    }
    let database=tokio::time::timeout(Duration::from_secs(2),database(&state.runtime)).await
        .unwrap_or_else(|_|json!({"status":"unknown","error":"Database observation deadline exceeded","main_file":null}));
    let mut paths=vec![("research_artifacts",state.settings.research.directory.clone()),("simulation_reports",state.settings.reports.directory.clone()),
        ("commissioning_profile",state.settings.runtime.profiles_file.clone().unwrap_or_default())];
    if let Some(file)=database["main_file"].as_str(){paths.push(("runtime_database",file.to_owned()));paths.push(("database_wal",format!("{file}-wal")));}
    let path_values=if let Ok(permit)=FILESYSTEM_OBSERVERS.try_acquire() {
        let task=tokio::task::spawn_blocking(move||{let _permit=permit;paths.into_iter().map(|(name,path)|path_observation(name,&path)).collect::<Vec<_>>()});
        match tokio::time::timeout(Duration::from_secs(2),task).await {Ok(Ok(paths))=>json!(paths),_=>json!([{ "name":"local_storage","kind":"unknown","file_bytes":null,"filesystem":{"status":"unknown","total_bytes":null,"available_bytes":null,"reason":"Filesystem observation deadline exceeded"}}])}
    }else{json!([{ "name":"local_storage","kind":"unknown","file_bytes":null,"filesystem":{"status":"unknown","total_bytes":null,"available_bytes":null,"reason":"Filesystem observer busy"}}])};
    let (endpoint,endpoint_source)=tokio::time::timeout(Duration::from_secs(2),configured_endpoint(&state)).await
        .unwrap_or_else(|_|(state.settings.opcua.endpoint_url.clone(),"registry_observation_timed_out_settings_only"));
    let observations=state.runtime.observations.read().await;
    let mut observed:BTreeMap<String,(DateTime<Utc>,Option<DateTime<Utc>>)>=BTreeMap::new();
    for sample in observations.values() {
        let time=DateTime::parse_from_rfc3339(&sample.timestamp).ok().map(|time|time.with_timezone(&Utc));
        observed.entry(sample.source_endpoint.clone()).and_modify(|value|{value.0=value.0.max(sample.received_at);value.1=value.1.max(time);}).or_insert((sample.received_at,time));
    }
    drop(observations);
    let observed_endpoints:Vec<_>=observed.into_iter().map(|(endpoint,(received,source))|json!({"endpoint":endpoint,"last_received_at":received,"last_source_timestamp":source})).collect();
    let current=state.auth.get_user_auth_state(user.id).await.map_err(|_|invalid("Maintenance identity unavailable"))?
        .ok_or_else(||invalid("Maintenance identity no longer exists"))?;
    if current.disabled||current.force_password_change||current.role!=user.role||current.auth_version!=actor.auth_version{return Err(RuntimeError(StatusCode::FORBIDDEN,"Maintenance observation authorization changed".into()));}
    for controller in &mut controllers {
        if let Some(profile)=controller["profile_hash"].as_str().and_then(|hash|state.runtime.profiles.get(hash)) {
            if let Err(error)=safety::safe_native(profile,&controller["native_status"]) {
                let reason=format!("{}: {}",profile.engine_id,error.1);
                if !blockers.contains(&reason){blockers.push(reason);}
                if let Some(local)=controller["blockers"].as_array_mut(){if !local.iter().any(|value|value==&error.1){local.push(json!(error.1));}}
            }
        }
    }
    let endpoint=if endpoint_source=="registry_observation_timed_out_settings_only"{None}else{
        reqwest::Url::parse(&endpoint).ok().filter(|url|url.username().is_empty()&&url.password().is_none()&&url.query().is_none()&&url.fragment().is_none()).map(|_|endpoint)
    };
    if endpoint.is_none(){blockers.push("Configured endpoint unavailable or contains redacted credential/query components".into());}
    let worker=state.runtime.worker_connection().await;
    Ok(Json(json!({"observed_at":Utc::now(),"can_reconnect":blockers.is_empty(),"blockers":blockers,"controllers":controllers,
        "connection":{"configured_endpoint":endpoint,"endpoint_source":endpoint_source,"worker_configured_enabled":state.settings.runtime.connect_opcua,
            "worker_observation_available":!observed_endpoints.is_empty(),"observed_endpoints":observed_endpoints,
            "worker":worker,
            "command_channel_available_capacity":state.cmd_tx.capacity(),"command_channel_max_capacity":state.cmd_tx.max_capacity(),
            "reconnect_delay_seconds":state.settings.runtime.reconnect_delay_sec},
        "storage":{"database":database,"paths":path_values,"recursive_scan":false,"directory_total_bytes":null},
        "retention":{"runtime_events":null,"runtime_operations":null,"research_artifacts":null,"historian":null,"status":"not_configured_in_hmi",
            "reason":"No retention/deletion policy is exposed by the current HMI configuration; remote historian retention is not inferred"},
        "limits":{"research":state.research.maintenance_limits(),"profile_count":state.runtime.profiles.len(),
            "historian":{"enabled":state.historian_config.enabled,"batch_size":state.historian_config.batch_size,"flush_interval_ms":state.historian_config.flush_interval_ms,"snapshot_interval_sec":state.historian_config.snapshot_interval_sec},
            "postgres_sources":state.settings.research.postgres_sources.iter().map(|source|json!({"id":source.id,"max_rows":source.max_rows,"statement_timeout_seconds":source.statement_timeout_seconds})).collect::<Vec<_>>()},
        "reconnect":{"method":"POST","path":"/api/infrastructure/opc-reconnect","payload":null,"csrf_required":true,"completion":"queued_is_not_connected","revalidation_required":true}})))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_or_unconfigured_paths_never_report_zero_capacity() {
        let absent=path_observation("missing","/proc/authrity-nonexistent-resource");
        assert!(absent["file_bytes"].is_null());assert!(absent["filesystem"]["total_bytes"].is_null());
        let unspecified=path_observation("unconfigured","");
        assert_eq!(unspecified["kind"],"not_configured");assert!(unspecified["filesystem"]["available_bytes"].is_null());
    }
}
