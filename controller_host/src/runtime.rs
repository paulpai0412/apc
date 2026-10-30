//! Private, bounded native authority IPC; no browser path or PID is an actuation target.
use std::{path::{Path as FsPath,PathBuf},sync::Arc,io::Write};
use axum::{extract::{Path,State},http::StatusCode,response::{IntoResponse,Response},Json};
use serde_json::{json,Value};
use tokio::io::{AsyncBufReadExt,AsyncReadExt,AsyncWriteExt,BufReader};
use anyhow::{ensure,Context,Result};
use crate::{AppState,ProcessState,artifacts};

#[derive(Debug)]
pub struct Launch {pub socket:PathBuf,pub profile_path:PathBuf,pub profile:Value,pub startup_approval_path:Option<PathBuf>}
pub fn private_root(path:&FsPath)->Result<PathBuf> {
    #[cfg(unix)] {
        use std::os::unix::fs::{DirBuilderExt,PermissionsExt};
        if !path.exists(){let mut builder=std::fs::DirBuilder::new();builder.recursive(true).mode(0o700).create(path)?;}
        let meta=std::fs::symlink_metadata(path)?;
        ensure!(meta.is_dir()&&!meta.file_type().is_symlink()&&meta.permissions().mode()&0o077==0,"Native runtime directory must be private (0700), not a symlink");
        Ok(path.canonicalize()?)
    }
    #[cfg(not(unix))] { let _=path;anyhow::bail!("Commissioned native authority requires Unix domain sockets on this deployment") }
}
impl Launch {
    pub fn prepare(root:&FsPath,id:&str,profile:&Value,model:&artifacts::LoadedModel,endpoint:&str,approval:Option<&Value>,recover_monitor:bool)->Result<Arc<Self>> {
        ensure!(profile["engine_id"]==id&&profile["endpoint"]==endpoint,"Commissioning scope/endpoint differs from launch target");
        if let Some(proof)=approval {
            ensure!(proof["profile_hash"]==profile["profile_hash"]&&proof["model_sha256"]==model.model_sha256
                &&model.control_version_id.as_deref()==proof["candidate_hash"].as_str(),"Startup approval differs from immutable model and commissioning profile");
        }else{ensure!(profile["model_sha256"]==model.model_sha256,"Start model differs from commissioned initial model");}
        ensure!(!recover_monitor||approval.is_none(),"Recovery Monitor cannot load an unconfirmed candidate");
        let mut recovery_locks:Vec<std::fs::File>=Vec::new();
        let active=root.join(format!("active-{id}.json"));
        if active.exists() {
            let previous:Value=serde_json::from_slice(&std::fs::read(&active)?)?;
            let previous_socket=previous["socket"].as_str().context("Invalid persisted authority identity")?;
            if FsPath::new(previous_socket).exists() {
                ensure!(recover_monitor,"Previous native authority remains; use explicit recovery only after inspecting the original operation");
                #[cfg(unix)] match std::os::unix::net::UnixStream::connect(previous_socket) {
                    Ok(_)=>anyhow::bail!("Existing native authority is reachable; recover against its current instance, never create another"),
                    Err(error) if matches!(error.kind(),std::io::ErrorKind::ConnectionRefused|std::io::ErrorKind::NotFound)=>{},
                    Err(error)=>return Err(error).context("Previous authority cannot be independently assessed"),
                }
                recovery_locks=probe_writer_locks(root,profile)?;
            }
        }
        let hash=profile["profile_hash"].as_str().context("Missing profile hash")?;
        ensure!(hash.len()==64&&hash.bytes().all(|b|b.is_ascii_hexdigit()),"Invalid commissioning hash");
        let variables=profile["variables"].as_array().context("Missing commissioned variable map")?;
        // Permanent ownership assignment, not a first-Engage race or a releasable browser lock.
        for mv in variables.iter().filter(|v|v["role"]=="MV") {
            let tag=mv["command_tag"].as_str().context("Missing commissioned MV command tag")?;
            let key=artifacts::digest(format!("{endpoint}\0{tag}").as_bytes());
            let owner_path=root.join(format!("owner-{key}.json"));
            let owner=json!({"engine_id":id,"endpoint":endpoint,"command_tag":tag,"equipment_instance_id":mv["equipment_instance_id"]});
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&owner_path) {
                Ok(mut file)=>{file.write_all(&serde_json::to_vec(&owner)?)?;file.sync_all()?;},
                Err(e) if e.kind()==std::io::ErrorKind::AlreadyExists=>{
                    let existing:Value=serde_json::from_slice(&std::fs::read(&owner_path)?)?;
                    ensure!(existing==owner,"Physical MV is assigned to another commissioned group; independent engineering handoff required");
                },Err(e)=>return Err(e.into()),
            }
        }
        let dir=root.join(uuid::Uuid::new_v4().simple().to_string());
        #[cfg(unix)] {use std::os::unix::fs::DirBuilderExt;std::fs::DirBuilder::new().mode(0o700).create(&dir)?;}
        #[cfg(not(unix))] {anyhow::bail!("Native authority transport unavailable");}
        let socket=dir.join("a.sock");ensure!(socket.as_os_str().len()<104,"Authority socket path is too long; configure a shorter private runtime directory");
        let profile_path=dir.join("profile.json");
        let mut file=std::fs::OpenOptions::new().create_new(true).write(true).open(&profile_path)?;
        file.write_all(&serde_json::to_vec(profile)?)?;file.sync_all()?;
        let startup_approval_path=if let Some(approval)=approval {
            let path=dir.join("startup-approval.json");
            let manifest=model.manifest_json.as_deref().context("Approved startup requires exact immutable manifest")?;
            let mut file=std::fs::OpenOptions::new().create_new(true).write(true).open(&path)?;
            file.write_all(&serde_json::to_vec(&json!({"approval":approval,"manifest_json":manifest}))?)?;file.sync_all()?;
            Some(path)
        }else{None};
        std::fs::File::open(&dir)?.sync_all()?;
        persist_receipt(&active,&json!({"socket":socket,"profile_path":profile_path}))?;
        // The new native must reacquire every same lock before any OPC connection/write.
        // Preserve the old socket, directory and journal; no PID or path deletion is evidence.
        drop(recovery_locks);
        Ok(Arc::new(Self{socket,profile_path,profile:profile.clone(),startup_approval_path}))
    }
    pub fn confirmed_exit(&self) {
        let Some(root)=self.socket.parent().and_then(FsPath::parent) else{return;};
        let Some(id)=self.profile["engine_id"].as_str() else{return;};
        let active=root.join(format!("active-{id}.json"));
        if let Ok(bytes)=std::fs::read(&active) {
            if serde_json::from_slice::<Value>(&bytes).ok().is_some_and(|v|v["socket"]==json!(self.socket)) {
                let _=std::fs::remove_file(&self.socket);
                let _=std::fs::remove_file(active);
                if let Ok(directory)=std::fs::File::open(root){let _=directory.sync_all();}
            }
        }
    }
}
fn launch(state:&AppState,id:&str)->Result<Arc<Launch>> {
    ensure!(artifacts::valid_id(id),"Invalid controller identity");
    let processes=state.processes.lock().map_err(|_|anyhow::anyhow!("Process registry unavailable"))?;
    if let Some(process)=processes.get(id) {
        ensure!(matches!(process.state,ProcessState::Running(_)),"Controller is not running");
        return process.authority.clone().context("Controller has no commissioned native authority channel");
    }
    drop(processes);
    // A host restart does not imply the independently owned engine stopped.
    let active:Value=serde_json::from_slice(&std::fs::read(state.runtime_dir.join(format!("active-{id}.json"))).context("Controller is not started")?)?;
    let socket=PathBuf::from(active["socket"].as_str().context("Missing persisted socket")?);
    let profile_path=PathBuf::from(active["profile_path"].as_str().context("Missing persisted profile")?);
    ensure!(socket.starts_with(&state.runtime_dir)&&profile_path.starts_with(&state.runtime_dir),"Persisted authority escapes private runtime root");
    let profile:Value=serde_json::from_slice(&std::fs::read(&profile_path)?)?;
    ensure!(profile["engine_id"]==id,"Persisted authority target mismatch");
    Ok(Arc::new(Launch{socket,profile_path,profile,startup_approval_path:None}))
}
async fn exchange(socket:&FsPath,request:&Value)->Result<Value> {
    #[cfg(unix)] {
        let bytes=serde_json::to_vec(request)?;ensure!(bytes.len()<64*1024,"Authority request exceeds 64 KiB");
        tokio::time::timeout(std::time::Duration::from_secs(if request["kind"]=="command"{310}else{3}),async {
            let mut stream=tokio::net::UnixStream::connect(socket).await.context("Native authority channel unavailable")?;
            stream.write_all(&bytes).await?;stream.write_all(b"\n").await?;stream.flush().await?;
            let mut reader=BufReader::new(stream).take(64*1024+1);let mut line=String::new();
            reader.read_line(&mut line).await?;
            ensure!(line.len()<=64*1024&&line.ends_with('\n'),"Native authority response exceeds limit or is incomplete");
            Ok(serde_json::from_str(&line)?)
        }).await.context("Native authority operation timed out; outcome unknown")?
    }
    #[cfg(not(unix))] {let _=(socket,request);anyhow::bail!("Native authority transport unavailable")}
}
fn failure(error:anyhow::Error)->Response {(StatusCode::CONFLICT,Json(json!({"ok":false,"detail":format!("{error:#}")}))).into_response()}
pub async fn status(State(state):State<AppState>,Path(id):Path<String>)->Response {
    let result=async {let launch=launch(&state,&id)?;exchange(&launch.socket,&json!({"kind":"status"})).await}.await;
    match result {Ok(value)=>Json(value).into_response(),Err(error)=>failure(error)}
}
fn operation_path(root:&FsPath,id:&str)->Result<PathBuf> {
    ensure!(!id.is_empty()&&id.len()<=128&&id.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-'||b==b'_'),"Invalid operation ID");
    Ok(root.join(format!("operation-{id}.json")))
}
fn persist_receipt(path:&FsPath,value:&Value)->Result<()> {
    let temp=path.with_extension(format!("{}.tmp",uuid::Uuid::new_v4().simple()));
    let mut file=std::fs::OpenOptions::new().create_new(true).write(true).open(&temp)?;
    file.write_all(&serde_json::to_vec(value)?)?;file.sync_all()?;
    std::fs::rename(temp,path)?;
    std::fs::File::open(path.parent().context("Missing operation directory")?)?.sync_all()?;
    Ok(())
}
pub(super) fn register_start(root:&FsPath,id:&str,operation_id:&str,profile:&Value,model_sha256:&str,approval:Option<&Value>,recover_monitor:bool)->Result<Option<Value>> {
    let path=operation_path(root,operation_id)?;
    let intent=json!({"kind":"start","profile_hash":profile["profile_hash"],"model_sha256":model_sha256,"approval":approval,"recover_monitor":recover_monitor});
    match std::fs::OpenOptions::new().create_new(true).write(true).open(&path) {
        Ok(mut file)=>{
            file.write_all(&serde_json::to_vec(&json!({"controller_id":id,"request":intent,"response":null}))?)?;
            file.sync_all()?;std::fs::File::open(root)?.sync_all()?;Ok(None)
        },
        Err(error) if error.kind()==std::io::ErrorKind::AlreadyExists=>{
            let receipt:Value=serde_json::from_slice(&std::fs::read(path)?)?;
            ensure!(receipt["controller_id"]==id&&receipt["request"]==intent,"Start operation ID already binds another intent");
            Ok(Some(if receipt["response"].is_null(){json!({"ok":false,"unknown":true,"operation_id":operation_id,"detail":"Start result unknown; no restart performed"})}else{receipt["response"].clone()}))
        },Err(error)=>Err(error.into()),
    }
}
pub(super) fn finish_start(root:&FsPath,operation_id:&str,response:Value)->Result<Value> {
    let path=operation_path(root,operation_id)?;
    let mut receipt:Value=serde_json::from_slice(&std::fs::read(&path)?)?;receipt["response"]=response.clone();
    persist_receipt(&path,&receipt)?;Ok(response)
}
pub(super) fn install(state:&AppState,request:&artifacts::InstallRequest)->Result<Value> {
    let model:Value=serde_json::from_str(&request.model_json)?;
    let id=model["metadata"]["name"].as_str().filter(|id|artifacts::valid_id(id)).context("Invalid installation controller")?;
    let path=operation_path(&state.runtime_dir,&request.operation_id)?;
    let intent=json!({"kind":"install","payload":request});
    let mut receipt=json!({"controller_id":id,"request":intent,"response":null});
    match std::fs::OpenOptions::new().create_new(true).write(true).open(&path) {
        Ok(mut file)=>{file.write_all(&serde_json::to_vec(&receipt)?)?;file.sync_all()?;std::fs::File::open(&state.runtime_dir)?.sync_all()?;},
        Err(error) if error.kind()==std::io::ErrorKind::AlreadyExists=>{
            let original:Value=serde_json::from_slice(&std::fs::read(path)?)?;
            ensure!(original["controller_id"]==id&&original["request"]==intent,"Installation operation ID already binds another intent");
            ensure!(!original["response"].is_null(),"Original installation remains unknown; no retry performed");
            return Ok(original["response"].clone());
        },Err(error)=>return Err(error.into()),
    }
    let installed=artifacts::install(&state.model_dir,request)?;
    let mut response=serde_json::to_value(installed)?;
    response["operation_id"]=json!(request.operation_id);
    response["profile_hash"]=request.approval["profile_hash"].clone();
    receipt["response"]=response.clone();persist_receipt(&path,&receipt)?;
    Ok(response)
}
pub async fn operation(State(state):State<AppState>,Path((id,operation_id)):Path<(String,String)>)->Response {
    let result:Result<Value>=async {
        let path=operation_path(&state.runtime_dir,&operation_id)?;
        let mut receipt:Value=serde_json::from_slice(&std::fs::read(&path).context("Operation is not registered")?)?;
        ensure!(receipt["controller_id"]==id,"Operation belongs to another controller");
        if receipt["response"].is_null()&&receipt["request"]["kind"]=="install" {
            let payload=&receipt["request"]["payload"];
            if let (Some(version),Some(profile_hash))=(payload["version_id"].as_str(),payload["approval"]["profile_hash"].as_str()) {
                if let Ok(Some(installed))=artifacts::original_installation(&state.model_dir,&id,version,&operation_id,profile_hash) {
                    let mut response=serde_json::to_value(installed)?;
                    response["operation_id"]=json!(operation_id);response["profile_hash"]=json!(profile_hash);
                    receipt["response"]=response;persist_receipt(&path,&receipt)?;
                }
            }
        }
        if receipt["response"].is_null() {
            if let Ok(launch)=launch(&state,&id) {
                if let Ok(response)=exchange(&launch.socket,&json!({"kind":"operation","operation_id":operation_id})).await {
                    if response["status"]["operation_id"]==operation_id {
                        receipt["response"]=response;persist_receipt(&path,&receipt)?;
                    }
                }
            }
        }
        if receipt["response"].is_null(){return Ok(json!({"ok":false,"unknown":true,"operation_id":operation_id,"detail":"Registered outcome remains unknown; do not resend"}));}
        Ok(receipt["response"].clone())
    }.await;
    match result{Ok(value)=>Json(value).into_response(),Err(error)=>failure(error)}
}
pub async fn command(State(state):State<AppState>,Path(id):Path<String>,Json(mut request):Json<Value>)->Response {
    let result:Result<Value>=async {
        let operation_id=request["operation_id"].as_str().context("Operation ID required")?.to_owned();
        let path=operation_path(&state.runtime_dir,&operation_id)?;
        let mut receipt=json!({"controller_id":id,"request":request,"response":null});
        match std::fs::OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(mut file)=>{file.write_all(&serde_json::to_vec(&receipt)?)?;file.sync_all()?;},
            Err(error) if error.kind()==std::io::ErrorKind::AlreadyExists=>{
                let existing:Value=serde_json::from_slice(&std::fs::read(&path)?)?;
                ensure!(existing["controller_id"]==id&&existing["request"]==request,"Operation ID already binds different intent");
                if !existing["response"].is_null(){return Ok(existing["response"].clone());}
                return Ok(json!({"ok":false,"unknown":true,"operation_id":operation_id,"detail":"Original dispatch is unresolved; query status, do not resend"}));
            },Err(error)=>return Err(error.into()),
        }
        std::fs::File::open(&state.runtime_dir)?.sync_all()?;
        let launch=launch(&state,&id)?;
        ensure!(request["profile_hash"]==launch.profile["profile_hash"],"Operation profile differs from running authority");
        let kind=request["action"]["kind"].as_str().context("Action kind required")?.to_owned();
        let mut switched=None;
        if kind=="switch_version" {
            let version=request["action"]["candidate_hash"].as_str().context("Exact approved candidate hash required")?.to_owned();
            ensure!(version.len()==64&&version.bytes().all(|b|b.is_ascii_hexdigit()),"Invalid candidate identity");
            let mut proof=request.as_object_mut().and_then(|value|value.remove("approval")).context("Independent approval proof required")?;
            ensure!(proof["profile_hash"]==launch.profile["profile_hash"]
                &&proof["target"]==launch.profile["target"]
                &&proof["operating_envelope"]==launch.profile["operating_envelope"],"Approval differs from running commissioned scope");
            if let Some(mut bundle)=proof.as_object_mut().and_then(|value|value.remove("version_bundle")) {
                let object=bundle.as_object_mut().context("Invalid immutable publication bundle")?;
                object.insert("approved_by".into(),request["action"]["approval_id"].clone());
                object.insert("operation_id".into(),json!(operation_id));
                object.insert("approval".into(),proof.clone());
                let installation:artifacts::InstallRequest=serde_json::from_value(bundle)?;
                ensure!(installation.version_id==version,"Publication bundle differs from handoff intent");
                let installed=artifacts::install(&state.model_dir,&installation)?;
                ensure!(installed.id==id,"Published model belongs to a different native controller");
            }
            let filename=format!("control-version-{version}.json");
            let model=artifacts::load_approved(&state.model_dir,&id,&filename,&proof)?;
            ensure!(model.control_version_id.as_deref()==Some(version.as_str()),"Handoff requires an immutable installed version");
            switched=Some((filename,model.model_sha256.clone(),version));
            let action=&mut request["action"];
            action["max_first_move"]=bounded_first_move(&model.bytes,&action["max_first_move"])?;
            action.as_object_mut().context("Invalid action")?.remove("candidate_hash");
            action["model_file"]=json!(model.path.canonicalize()?);
            action["approved_sha256"]=json!(model.model_sha256);
            action["approval_expires_at"]=proof["expires_at"].clone();
            request.as_object_mut().context("Invalid operation")?.remove("approval");
        }
        let response=exchange(&launch.socket,&json!({"kind":"command","request":request})).await?;
        receipt["response"]=response.clone();
        persist_receipt(&path,&receipt)?;
        if response["ok"]==true&&response["confirmed"]==true {
            if let Some((filename,hash,version))=switched {
                ensure!(response["status"]["model_sha256"]==hash,"Native switch receipt model digest mismatch");
                if let Some(process)=state.processes.lock().map_err(|_|anyhow::anyhow!("Process registry unavailable"))?.get_mut(&id) {
                    process.active_model=Some(filename);process.model_sha256=Some(hash);process.control_version_id=Some(version);
                }
            }
        }
        if response["ok"]==true&&kind=="stop" {
            ensure!(response["status"]["mode"]=="stopped"&&response["status"]["dcs_handoff"]==true
                &&response["status"]["pending_application"]==false,"Native stop has not proven command cessation and DCS handoff");
            let sender=state.processes.lock().map_err(|_|anyhow::anyhow!("Process registry unavailable"))?.get(&id)
                .and_then(|p|p.stop_sender.clone()).context("Owning process monitor is unavailable")?;
            let(reply,done)=tokio::sync::oneshot::channel();sender.send(reply).await.context("Owning process monitor closed")?;
            done.await.context("Process stop result unknown")??;
        }
        Ok(response)
    }.await;
    match result {Ok(value)=>Json(value).into_response(),Err(error)=>failure(error)}
}

fn bounded_first_move(model:&[u8],proposed:&Value)->Result<Value> {
    let model:Value=serde_json::from_slice(model)?;
    let variables=model["variables"]["mvs"].as_array().context("Candidate MV order unavailable")?;
    let proposed=proposed.as_array().context("First-move bounds required")?;
    ensure!(variables.len()==proposed.len(),"Candidate first-move dimensions changed");
    let bounds:Result<Vec<f64>>=variables.iter().zip(proposed).map(|(variable,limit)|{
        let candidate=variable["max_move"].as_f64().filter(|v|v.is_finite()&&*v>0.0).context("Invalid candidate move bound")?;
        let limit=limit.as_f64().filter(|v|v.is_finite()&&*v>0.0).context("Invalid approval first-move bound")?;
        Ok(candidate.min(limit))
    }).collect();
    Ok(json!(bounds?))
}

#[cfg(test)]
mod authority_tests {
    use super::*;
    #[test]
    fn registered_start_cannot_change_recovery_authority() {
        for recovery in [false,true] {
            let root=tempfile::tempdir().unwrap();
            let profile=json!({"profile_hash":"a".repeat(64)});
            register_start(root.path(),"mill","start-1",&profile,"model",None,recovery).unwrap();
            assert!(register_start(root.path(),"mill","start-1",&profile,"model",None,!recovery).is_err());
            let original=register_start(root.path(),"mill","start-1",&profile,"model",None,recovery).unwrap().unwrap();
            assert_eq!(original["unknown"],true);
            assert_eq!(original["ok"],false);
        }
    }

    #[test]
    fn first_move_respects_candidate_and_approval_bounds_in_exact_order() {
        let model=br#"{"variables":{"mvs":[{"max_move":0.25},{"max_move":3.0}]}}"#;
        assert_eq!(bounded_first_move(model,&json!([1.0,0.5])).unwrap(),json!([0.25,0.5]));
        assert!(bounded_first_move(model,&json!([1.0])).is_err());
        assert!(bounded_first_move(model,&json!([1.0,-0.5])).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn recovery_probes_reject_a_live_physical_owner_and_preserve_lock_files() {
        let root=tempfile::tempdir().unwrap();
        let profile=json!({"endpoint":"opc.tcp://127.0.0.1:4840","variables":[{"role":"MV","command_tag":"feed"},{"role":"MV","command_tag":"fan"}]});
        let owner=probe_writer_locks(root.path(),&profile).unwrap();
        assert_eq!(owner.len(),2);
        assert!(probe_writer_locks(root.path(),&profile).is_err());
        drop(owner);
        let next=probe_writer_locks(root.path(),&profile).unwrap();
        assert_eq!(next.len(),2);
        let key=artifacts::digest(b"opc.tcp://127.0.0.1:4840\0feed");
        assert!(root.path().join(format!("writer-{key}.lock")).is_file());
    }
}

fn probe_writer_locks(root:&FsPath,profile:&Value)->Result<Vec<std::fs::File>> {
    use cap_fs_ext::OpenOptionsFollowExt;
    let endpoint=profile["endpoint"].as_str().context("Missing physical endpoint")?;
    let variables=profile["variables"].as_array().context("Missing physical ownership map")?;
    let directory=cap_std::fs::Dir::open_ambient_dir(root,cap_std::ambient_authority())?;
    let mut held=Vec::new();
    for variable in variables.iter().filter(|variable|variable["role"]=="MV") {
        let tag=variable["command_tag"].as_str().context("Missing physical MV command tag")?;
        let key=artifacts::digest(format!("{endpoint}\0{tag}").as_bytes());
        let mut options=cap_std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).follow(cap_fs_ext::FollowSymlinks::No);
        let file=directory.open_with(format!("writer-{key}.lock"),&options)?.into_std();
        ensure!(file.metadata()?.is_file(),"Physical ownership lock is not a regular file");
        fs2::FileExt::try_lock_exclusive(&file).context("A native process still owns a commissioned MV; recovery cannot proceed")?;
        #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;file.set_permissions(std::fs::Permissions::from_mode(0o600))?;}
        held.push(file);
    }
    ensure!(!held.is_empty(),"No commissioned MV ownership evidence");
    Ok(held)
}
