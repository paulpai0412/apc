//! Administrator-selected supervisor transport. Never follows redirects or accepts browser URLs.
use serde_json::{json,Value};
use crate::state::AppState;
use super::{invalid,RuntimeError,Result,DeploymentProfile,OperationRequest,OperationAction};
use axum::http::StatusCode;

async fn request(state:&AppState,path:&str,body:Option<&Value>)->Result<Value> {
    let base=state.settings.services.supervisor_url.trim_end_matches('/');
    let url=reqwest::Url::parse(base).map_err(|_|invalid("Invalid configured supervisor URL"))?;
    if !matches!(url.scheme(),"http"|"https")||!url.username().is_empty()||url.password().is_some()||url.query().is_some()||url.fragment().is_some(){return Err(invalid("Supervisor URL must not contain credentials/query/fragment"));}
    let client=reqwest::Client::builder().redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(if body.is_some(){315}else{4}))
        .build().map_err(|_|invalid("Cannot create supervisor transport"))?;
    let request=if let Some(body)=body{client.post(format!("{base}{path}")).json(body)}else{client.get(format!("{base}{path}"))};
    let mut response=request.header("x-api-key",&state.settings.services.supervisor_api_key).send().await
        .map_err(|_|RuntimeError(StatusCode::BAD_GATEWAY,"Supervisor response unavailable; dispatched operation outcome may be unknown".into()))?;
    let status=response.status();let mut bytes=Vec::with_capacity(4096);
    while let Some(chunk)=response.chunk().await.map_err(|_|invalid("Supervisor response interrupted; outcome unknown"))? {
        if bytes.len()+chunk.len()>128*1024{return Err(invalid("Supervisor response exceeds bounded contract"));}bytes.extend_from_slice(&chunk);
    }
    let value:Value=serde_json::from_slice(&bytes).map_err(|_|invalid("Supervisor response is not valid JSON; outcome unknown"))?;
    if !status.is_success(){return Err(RuntimeError(StatusCode::BAD_GATEWAY,value["detail"].as_str().unwrap_or("Supervisor did not confirm operation").to_owned()));}
    Ok(value)
}
fn controller_path(profile:&DeploymentProfile)->Result<String> {
    if profile.engine_id.is_empty()||!profile.engine_id.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'_'||b==b'-') {return Err(invalid("Unsafe commissioned controller identifier"));}
    Ok(format!("/api/controllers/{}",profile.engine_id))
}
pub(super) async fn status(state:&AppState,profile:&DeploymentProfile)->Result<Value> {
    let response=request(state,&format!("{}/runtime",controller_path(profile)?),None).await?;
    if response["ok"]!=true||response["status"]["profile_hash"]!=profile.profile_hash{return Err(invalid("Native status does not attest selected commissioning profile"));}
    Ok(response["status"].clone())
}
pub(super) async fn dispatch(state:&AppState,profile:&DeploymentProfile,intent:&OperationRequest,approval:Option<Value>)->Result<Value> {
    if profile.commissioning_kind=="synthetic_local"&&!state.settings.web.local_test_mode {
        return Err(invalid("Synthetic commissioning dispatch requires explicitly configured local test mode"));
    }
    let base=controller_path(profile)?;
    if matches!(intent.action,OperationAction::Start{}|OperationAction::RecoverMonitor{}|OperationAction::StartCandidate{..}) {
        let candidate_file=match &intent.action {OperationAction::StartCandidate{candidate_hash,..}=>Some(format!("control-version-{candidate_hash}.json")),_=>None};
        let model_file=candidate_file.as_deref().unwrap_or(&profile.model_file);
        let expected_model=if matches!(intent.action,OperationAction::StartCandidate{..}) {
            approval.as_ref().and_then(|value|value["model_sha256"].as_str()).ok_or_else(||invalid("Approved candidate model identity is missing"))?
        }else{profile.model_sha256.as_str()};
        let payload=json!({"operation_id":intent.operation_id,"expires_at":intent.expires_at,"recover_monitor":matches!(intent.action,OperationAction::RecoverMonitor{}),"model_filename":model_file,"commissioning_profile":profile,"approval":approval,
            "opc_url":profile.endpoint,"acquisition_sequence_node":profile.acquisition_sequence_node,
            "security_policy":state.settings.opcua.security_policy,"message_mode":state.settings.opcua.message_mode,
            "auth_mode":state.settings.auth.mode.as_str(),"username":state.settings.auth.username,"password":state.settings.auth.password,
            "live_tuning":false});
        let response=request(state,&format!("{base}/start"),Some(&payload)).await?;
        if response["operation_id"]!=intent.operation_id||response["profile_hash"]!=profile.profile_hash||response["unknown"]==true||response["ok"].as_bool().is_none()
            ||(response["ok"]==true&&(response["model_sha256"]!=expected_model
                ||matches!(&intent.action,OperationAction::StartCandidate{candidate_hash,..} if response["control_version_id"]!=candidate_hash.as_str()))) {
            return Err(invalid("Supervisor start result does not bind this operation; outcome unknown"));
        }
        return Ok(response);
    }
    let mut payload=serde_json::to_value(intent).map_err(super::storage)?;
    if matches!(intent.action,OperationAction::Monitor{}) {payload["action"]=json!({"kind":"start"});}
    if let Some(approval)=approval {
        payload["approval"]=approval;
        payload["action"]["max_first_move"]=json!(profile.variables.iter().filter(|v|v.role=="MV").map(|v|v.max_move).collect::<Vec<_>>());
    }
    let response=request(state,&format!("{base}/runtime"),Some(&payload)).await?;
    if response["unknown"]==true||response["ok"].as_bool().is_none()||response["status"]["profile_hash"]!=profile.profile_hash
        || response["status"]["operation_id"]!=intent.operation_id
        || response["status"]["instance_id"].as_str()!=intent.expected_instance.as_deref() {
        return Err(invalid("Native response does not bind the original operation and profile; outcome unknown"));
    }
    Ok(response)
}
pub(super) async fn original(state:&AppState,profile:&DeploymentProfile,intent:&OperationRequest)->Result<Value> {
    if intent.operation_id.is_empty()||!intent.operation_id.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-'||b==b'_') {
        return Err(invalid("Invalid operation identifier"));
    }
    let response=request(state,&format!("{}/runtime/operations/{}",controller_path(profile)?,intent.operation_id),None).await?;
    if response["unknown"]==true||response["ok"].as_bool().is_none(){return Err(invalid("Original operation result remains unknown"));}
    if matches!(intent.action,OperationAction::Start{}|OperationAction::RecoverMonitor{}|OperationAction::StartCandidate{..}) {
        if response["operation_id"]!=intent.operation_id||response["profile_hash"]!=profile.profile_hash{return Err(invalid("Start receipt identity mismatch"));}
        if response["ok"]==true&&match &intent.action {
            OperationAction::StartCandidate{candidate_hash,..}=>response["control_version_id"]!=candidate_hash.as_str(),
            _=>response["model_sha256"]!=profile.model_sha256,
        }{return Err(invalid("Start receipt model identity mismatch"));}
    }else if response["status"]["operation_id"]!=intent.operation_id||response["status"]["profile_hash"]!=profile.profile_hash
        ||response["status"]["instance_id"].as_str()!=intent.expected_instance.as_deref() {
        return Err(invalid("Original native operation identity mismatch"));
    }
    Ok(response)
}

pub(super) async fn original_installation(state:&AppState,profile:&DeploymentProfile,operation_id:&str,version:&str,model_sha256:&str,manifest_sha256:&str)->Result<Value> {
    if operation_id.is_empty()||!operation_id.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-'||b==b'_') {
        return Err(invalid("Invalid installation operation identity"));
    }
    let response=request(state,&format!("/api/controllers/{}/runtime/operations/{operation_id}",profile.engine_id),None).await?;
    if response["operation_id"]!=operation_id||response["profile_hash"]!=profile.profile_hash
        ||response["control_version_id"]!=version||response["model_sha256"]!=model_sha256
        ||response["manifest_sha256"]!=manifest_sha256||response["id"]!=profile.engine_id {
        return Err(invalid("Original installation outcome remains unknown"));
    }
    Ok(response)
}
