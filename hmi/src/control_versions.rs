//! Explicit installation of a verified research artifact; never starts control or creates OPC nodes.
use std::time::Duration;

use axum::{extract::{FromRequestParts, Path, State}, http::{request::Parts, StatusCode}, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{auth::{AuthError, CsrfVerified, CurrentUser}, research::{invalid, ResearchError, ResearchResult}, state::AppState};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrozenVersionBundle {
    pub version_id: String,
    pub model_sha256: String,
    pub model_json: String,
    pub manifest_json: String,
    pub manifest_sha256: String,
}

fn digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

impl FrozenVersionBundle {
    pub(crate) fn verify(&self, requested_id: &str) -> ResearchResult<()> {
        if self.version_id != requested_id || !digest(&self.version_id) || !digest(&self.model_sha256)
            || !digest(&self.manifest_sha256) || hash(&self.model_json) != self.model_sha256
            || hash(&self.manifest_json) != self.manifest_sha256 {
            return Err(invalid("Frozen artifact identity or byte digest mismatch."));
        }
        let manifest: Value = serde_json::from_str(&self.manifest_json)
            .map_err(|_| invalid("Invalid frozen version manifest."))?;
        if manifest["schema_version"] != 1 || manifest["id"] != self.version_id
            || manifest["model_sha256"] != self.model_sha256 || manifest["authority"] != "review_only"
            || manifest["assessment"]["eligible"] != true || manifest["assessment"]["reasons"] != json!([])
            || manifest["model_test"]["passed"] != true || manifest["model_test"]["used_for_selection"] != false {
            return Err(invalid("Frozen version has not qualified for explicit installation review."));
        }
        Ok(())
    }

    fn controller_id(&self) -> ResearchResult<String> {
        let model: Value = serde_json::from_str(&self.model_json).map_err(|_| invalid("Invalid frozen model JSON."))?;
        let id = model["metadata"]["name"].as_str().ok_or_else(|| invalid("Frozen model has no controller identifier."))?;
        if id.is_empty() || id.len() > 128 || !id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-') {
            return Err(invalid("Model identity must be an exact, safe controller namespace; it cannot be silently renamed."));
        }
        Ok(id.to_owned())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InstallationApproval {
    operation_id: String,
    profile_hash: String,
    approval_id: String,
    expected_epoch: i64,
    expires_at: chrono::DateTime<chrono::Utc>,
    reason: String,
}

#[derive(Serialize)]
struct InstallationRequest<'a> {
    #[serde(flatten)]
    bundle: &'a FrozenVersionBundle,
    approved_by: &'a str,
    operation_id:&'a str,
    approval:&'a Value,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InstallationReceipt {
    id: String,
    model_filename: String,
    model_sha256: String,
    control_version_id: String,
    manifest_sha256: String,
    operation_id:String,
    profile_hash:String,
}

impl InstallationReceipt {
    fn verify(&self, bundle: &FrozenVersionBundle, controller_id: &str,operation_id:&str,profile_hash:&str) -> ResearchResult<()> {
        if self.operation_id!=operation_id||self.profile_hash!=profile_hash||self.id != controller_id || self.control_version_id != bundle.version_id
            || self.model_sha256 != bundle.model_sha256 || self.manifest_sha256 != bundle.manifest_sha256
            || self.model_filename != format!("control-version-{}.json", bundle.version_id) {
            return Err(ResearchError(StatusCode::BAD_GATEWAY,
                "Supervisor receipt differs from the selected artifact. Inspect its stored state before any start or retry.".into()));
        }
        Ok(())
    }
}

fn supervisor_url(state: &AppState) -> ResearchResult<String> {
    let value = state.settings.services.supervisor_url.trim_end_matches('/');
    let url = reqwest::Url::parse(value).map_err(|_| invalid("Invalid administrator-configured supervisor URL."))?;
    if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some()
        || url.query().is_some() || url.fragment().is_some() || url.host_str().is_none() {
        return Err(invalid("Supervisor URL must be HTTP(S), without credentials, query or fragment."));
    }
    Ok(value.to_owned())
}

struct Installer(CurrentUser);

#[axum::async_trait]
impl FromRequestParts<AppState> for Installer {
    type Rejection = ResearchError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ResearchResult<Self> {
        let user = CurrentUser::from_request_parts(parts, state).await.map_err(|error| match error {
            AuthError::Unauthorized => ResearchError(StatusCode::UNAUTHORIZED, "Sign in before reviewing installations.".into()),
            AuthError::Forbidden => ResearchError(StatusCode::FORBIDDEN, "Installation access denied.".into()),
            _ => ResearchError(StatusCode::INTERNAL_SERVER_ERROR, "Cannot validate the login session.".into()),
        })?;
        if !matches!(user.role, crate::auth::Role::Engineer | crate::auth::Role::Admin) {
            return Err(ResearchError(StatusCode::FORBIDDEN, "Installation requires an engineer or administrator account.".into()));
        }
        Ok(Self(user))
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/control/versions/:id/install", post(install))
        .route("/api/control/versions/:id/reconcile",post(reconcile_installation))
}


async fn install(State(state): State<AppState>, Installer(user): Installer, _: CsrfVerified,
                 Path(id): Path<String>, Json(approval): Json<InstallationApproval>) -> ResearchResult<Json<crate::runtime::OperationRecord>> {
    if !digest(&id) || approval.reason.trim().is_empty() || approval.reason.len()>1024 {
        return Err(invalid("Select a frozen version and provide an installation reason."));
    }
    if state.settings.research.enabled {
        return Err(ResearchError(StatusCode::FORBIDDEN,"An isolated research deployment cannot install field versions.".into()));
    }
    // Deny uncommissioned or unauthorized targets before reading any frozen artifact.
    state.runtime.profile(&approval.profile_hash, &user, "runtime.deploy")
        .map_err(|error| ResearchError(error.0, error.1))?;
    let target=supervisor_url(&state)?;
    let bundle=state.research.version_bundle(&id).await?;
    bundle.verify(&id)?;
    let controller_id=bundle.controller_id()?;
    let intent=json!({"kind":"install","version_id":id,"approval":approval});
    let actor_version=state.auth.get_user_auth_state(user.id).await.map_err(|_|invalid("Installer identity unavailable"))?
        .ok_or_else(||invalid("Installer identity no longer exists"))?.auth_version;
    let (record,dispatch)=state.runtime.installation_intent(&user,&approval.profile_hash,&approval.approval_id,
        &approval.operation_id,approval.expected_epoch,approval.expires_at,intent,&bundle).await
        .map_err(|e|ResearchError(e.0,e.1))?;
    if !dispatch {return Ok(Json(record));}
    tokio::spawn(async move {
        let mut dispatched=false;
        let result:ResearchResult<InstallationReceipt>=async {
            let current=state.auth.get_user_auth_state(user.id).await.map_err(|_|invalid("Installer identity unavailable"))?
                .ok_or_else(||invalid("Installer identity no longer exists"))?;
            if current.disabled||current.force_password_change||current.role!=user.role||current.auth_version!=actor_version||approval.expires_at<=chrono::Utc::now(){
                return Err(invalid("Installation authorization revoked or expired before dispatch"));
            }
            let profile=state.runtime.profile(&approval.profile_hash,&user,"runtime.deploy").map_err(|error|ResearchError(error.0,error.1))?;
            let binding=state.runtime.approved_binding(profile,&id,&approval.approval_id).await.map_err(|error|ResearchError(error.0,error.1))?;
            let client=reqwest::Client::builder().redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30)).build()
                .map_err(|_|ResearchError(StatusCode::INTERNAL_SERVER_ERROR,"Cannot initialize supervisor transport".into()))?;
            dispatched=true;
            let mut response=client.post(format!("{target}/api/control-versions"))
                .header("x-api-key",&state.settings.services.supervisor_api_key)
                .json(&InstallationRequest{bundle:&bundle,approved_by:&approval.approval_id,operation_id:&approval.operation_id,approval:&binding}).send().await
                .map_err(|_|ResearchError(StatusCode::BAD_GATEWAY,"Installation outcome unknown; query original operation before any retry".into()))?;
            if !response.status().is_success(){
                return Err(ResearchError(StatusCode::BAD_GATEWAY,format!("Installation not confirmed (HTTP {})",response.status())));
            }
            let mut bytes=Vec::with_capacity(1024);
            while let Some(chunk)=response.chunk().await.map_err(|_|ResearchError(StatusCode::BAD_GATEWAY,"Installation response interrupted".into()))? {
                if bytes.len()+chunk.len()>32*1024{return Err(ResearchError(StatusCode::BAD_GATEWAY,"Oversized installation receipt".into()));}
                bytes.extend_from_slice(&chunk);
            }
            let receipt:InstallationReceipt=serde_json::from_slice(&bytes)
                .map_err(|_|ResearchError(StatusCode::BAD_GATEWAY,"Invalid installation receipt".into()))?;
            receipt.verify(&bundle,&controller_id,&approval.operation_id,&approval.profile_hash)?;
            Ok(receipt)
        }.await;
        let detail=match &result {
            Ok(receipt)=>serde_json::to_string(receipt).unwrap_or_else(|_|"Receipt serialization failed".into()),
            Err(error)=>error.1.clone(),
        };
        let outcome=if result.is_ok(){"accepted"}else if dispatched{"unknown"}else{"rejected"};
        if let Err(error)=state.runtime.installation_result(&approval.operation_id,outcome,&detail).await {
            tracing::error!("Installation registry outcome not durable: {}",error.1);
        }
        state.auth.audit(Some(&user),"control.version.install",Some(&id),
            outcome,Some(&detail)).await;
    });
    Ok(Json(record))
}

async fn reconcile_installation(State(state):State<AppState>,Installer(user):Installer,_:CsrfVerified,
    Path(id):Path<String>,Json(approval):Json<InstallationApproval>)->ResearchResult<Json<crate::runtime::OperationRecord>> {
    if !digest(&id){return Err(invalid("Invalid frozen version identity"));}
    let intent=json!({"kind":"install","version_id":id,"approval":approval});
    state.runtime.reconcile_intent(&user,&approval.operation_id,&approval.profile_hash,"runtime.deploy",intent,approval.expires_at)
        .await.map(Json).map_err(|e|ResearchError(e.0,e.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact() -> FrozenVersionBundle {
        let id = "a".repeat(64);
        let model_json = "{\n  \"metadata\": {\"name\": \"Mill_1\"}\n}".to_owned();
        let model_sha256 = hash(&model_json);
        let manifest_json = json!({"schema_version":1,"id":id,"model_sha256":model_sha256,
            "authority":"review_only","assessment":{"eligible":true,"reasons":[]},
            "model_test":{"passed":true,"used_for_selection":false}}).to_string();
        FrozenVersionBundle { version_id:id, model_sha256, model_json, manifest_sha256:hash(&manifest_json), manifest_json }
    }

    #[test]
    fn changed_bytes_and_unqualified_evidence_cannot_be_installed() {
        let mut bundle = artifact();
        bundle.verify(&bundle.version_id).unwrap();
        bundle.model_json.push(' ');
        assert!(bundle.verify(&bundle.version_id).is_err());
        bundle.model_sha256 = hash(&bundle.model_json);
        assert!(bundle.verify(&bundle.version_id).is_err());
        let mut bundle = artifact();
        let mut manifest: Value = serde_json::from_str(&bundle.manifest_json).unwrap();
        manifest["assessment"]["eligible"] = json!(false);
        bundle.manifest_json = manifest.to_string();
        bundle.manifest_sha256 = hash(&bundle.manifest_json);
        assert!(bundle.verify(&bundle.version_id).is_err());
    }

    #[test]
    fn receipt_must_bind_every_installed_identity() {
        let bundle = artifact();
        let receipt = || InstallationReceipt { id:"Mill_1".into(),
            model_filename:format!("control-version-{}.json",bundle.version_id), model_sha256:bundle.model_sha256.clone(),
            control_version_id:bundle.version_id.clone(), manifest_sha256:bundle.manifest_sha256.clone(),
            operation_id:"install-1".into(),profile_hash:"p".repeat(64) };
        receipt().verify(&bundle, "Mill_1","install-1",&"p".repeat(64)).unwrap();
        for field in 0..7 {
            let mut invalid_receipt = receipt();
            match field { 0 => invalid_receipt.id.push('x'), 1 => invalid_receipt.model_filename.push('x'),
                2 => invalid_receipt.model_sha256 = "b".repeat(64), 3 => invalid_receipt.control_version_id = "b".repeat(64),
                4 => invalid_receipt.manifest_sha256 = "b".repeat(64),
                5 => invalid_receipt.operation_id.push('x'), _ => invalid_receipt.profile_hash.push('x') }
            assert!(invalid_receipt.verify(&bundle, "Mill_1","install-1",&"p".repeat(64)).is_err());
        }
    }
}
