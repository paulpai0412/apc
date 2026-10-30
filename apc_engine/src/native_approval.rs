//! Verify immutable candidate provenance without changing the commissioning profile.
use anyhow::{ensure, Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::Path};

fn hash(value: &Value) -> bool {
    value.as_str().is_some_and(|value| value.len() == 64
        && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
}

pub fn load(path: &Path, profile: &Value, model_sha256: &str) -> Result<()> {
    ensure!(path.is_absolute() && std::fs::metadata(path)?.len() <= 4 * 1024 * 1024,
        "Startup approval must be an absolute bounded supervisor artifact");
    validate(&serde_json::from_slice(&std::fs::read(path)?)?, profile, model_sha256)
}

pub fn validate(bundle: &Value, profile: &Value, model_sha256: &str) -> Result<()> {
    let proof = &bundle["approval"];
    let manifest_json = bundle["manifest_json"].as_str().context("Exact immutable manifest bytes missing from startup approval")?;
    let manifest: Value = serde_json::from_str(manifest_json)?;
    ensure!(matches!(proof["kind"].as_str(), Some("manual_review" | "bounded_policy")), "Unknown candidate approval authority");
    ensure!(hash(&proof["candidate_hash"]) && hash(&proof["manifest_sha256"])
        && proof["model_sha256"] == model_sha256 && manifest["model_sha256"] == model_sha256
        && proof["candidate_hash"] == manifest["id"]
        && proof["manifest_sha256"] == format!("{:x}", Sha256::digest(manifest_json.as_bytes())),
        "Startup candidate/manifest byte identity mismatch");
    ensure!(proof["profile_hash"] == profile["profile_hash"] && hash(&proof["profile_hash"])
        && manifest["deployment_profile_hash"] == profile["profile_hash"], "Startup commissioning profile identity mismatch");
    ensure!(manifest["schema_version"] == 1 && manifest["authority"] == "review_only"
        && manifest.pointer("/assessment/eligible") == Some(&Value::Bool(true))
        && manifest.pointer("/assessment/reasons").and_then(Value::as_array).is_some_and(Vec::is_empty)
        && manifest.pointer("/model_test/passed") == Some(&Value::Bool(true))
        && manifest.pointer("/model_test/used_for_selection") == Some(&Value::Bool(false)),
        "Startup model lacks qualified held-out evidence");
    ensure!(proof["author_id"].as_i64().is_some_and(|id| id > 0)
        && proof["approver_id"].as_i64().is_some_and(|id| id > 0)
        && proof["author_id"] != proof["approver_id"] && manifest["author_user_id"] == proof["author_id"],
        "Independent candidate author and approver required");
    let realm = proof["author_identity_realm"].as_str().filter(|value| !value.is_empty()).context("Author identity realm missing")?;
    ensure!(proof["approver_identity_realm"] == realm && manifest["author_identity_realm"] == realm,
        "Candidate approval identity realms differ");
    ensure!(proof["target"].is_object() && proof["target"] == profile["target"] && manifest["target"] == profile["target"]
        && proof["operating_envelope"].as_str().is_some_and(|value| !value.is_empty())
        && proof["operating_envelope"] == profile["operating_envelope"] && manifest["operating_envelope"] == profile["operating_envelope"],
        "Candidate target or operating envelope differs from commissioning");
    let equipment: BTreeSet<String> = serde_json::from_value(profile["equipment_instance_ids"].clone())?;
    let approved: BTreeSet<String> = serde_json::from_value(proof["equipment_instance_ids"].clone())?;
    let candidate: BTreeSet<String> = serde_json::from_value(manifest["equipment_instance_ids"].clone())?;
    ensure!(!equipment.is_empty() && approved == equipment && candidate == equipment, "Candidate equipment identity differs from commissioning");
    let expiry = chrono::DateTime::parse_from_rfc3339(proof["expires_at"].as_str().context("Startup approval expiry missing")?)?;
    ensure!(expiry > chrono::Utc::now(), "Startup candidate approval expired");
    Ok(())
}
