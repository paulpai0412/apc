use anyhow::{ensure, Context, Result};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{io::{Read, Write}, path::{Path, PathBuf}};

const VERSIONS: &str = ".control-versions";
const PREFIX: &str = "control-version-";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallRequest {
    pub version_id: String,
    pub model_sha256: String,
    pub model_json: String,
    pub manifest_json: String,
    pub manifest_sha256: String,
    pub approved_by: String,
    pub operation_id:String,
    pub approval:Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InstalledVersion {
    pub id: String,
    pub model_filename: String,
    pub model_sha256: String,
    pub control_version_id: String,
    pub manifest_sha256: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema_version: u32,
    installation: InstalledVersion,
    approved_by: String,
    approval:Value,
    operation_id:String,
    installed_at: String,
    authority: String,
}

pub struct LoadedModel {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub model_sha256: String,
    pub control_version_id: Option<String>,
    pub manifest_json:Option<String>,
}

pub fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn valid_filename(value: &str) -> bool {
    !value.is_empty() && value.len() <= 240 && !value.starts_with('.')
        && value.ends_with(".json")
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn model_id(bytes: &[u8]) -> Result<String> {
    let model: Value = serde_json::from_slice(bytes).context("invalid model JSON")?;
    let id = model.pointer("/metadata/name").and_then(Value::as_str)
        .context("model metadata.name is required")?;
    ensure!(valid_id(id), "metadata.name must be an exact safe controller identifier");
    Ok(id.to_owned())
}

fn validate_manifest(bytes: &[u8], version: &str, model_hash: &str) -> Result<()> {
    let manifest: Value = serde_json::from_slice(bytes).context("invalid manifest JSON")?;
    ensure!(manifest["schema_version"] == 1, "unsupported manifest schema");
    ensure!(manifest["id"].as_str() == Some(version), "manifest id mismatch");
    if let Some(id) = manifest.get("version_id") {
        ensure!(id.as_str() == Some(version), "manifest version_id mismatch");
    }
    ensure!(manifest["model_sha256"].as_str() == Some(model_hash), "manifest model hash mismatch");
    ensure!(manifest["authority"] == "review_only", "manifest authority must be review_only");
    ensure!(manifest.pointer("/assessment/eligible") == Some(&Value::Bool(true)), "candidate assessment is not eligible");
    ensure!(manifest.pointer("/assessment/reasons").and_then(Value::as_array).is_some_and(|v| v.is_empty()), "candidate has missing or nonempty rejection reasons");
    ensure!(manifest.pointer("/model_test/passed") == Some(&Value::Bool(true)), "model test did not pass");
    ensure!(manifest.pointer("/model_test/used_for_selection") == Some(&Value::Bool(false)), "model test must be held out of selection");
    Ok(())
}

fn open_root(root: &Path) -> Result<Dir> {
    // Configuration owns the root; no request can choose it. Reject a linked root too.
    ensure!(!std::fs::symlink_metadata(root)?.file_type().is_symlink(), "model root is a symlink");
    Ok(Dir::open_ambient_dir(root, cap_std::ambient_authority())?)
}

fn child_dir(parent: &Dir, name: &str, create: bool) -> Result<Dir> {
    if create {
        match parent.create_dir(name) {
            Ok(()) => sync_dir(parent)?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {},
            Err(e) => return Err(e.into()),
        }
    }
    Ok(parent.open_dir_nofollow(name)?)
}

fn sync_dir(dir: &Dir) -> Result<()> {
    // A cloned capability may retain O_PATH, which cannot be fsynced on Linux.
    // Reopen "." relative to the pinned directory, without re-resolving its pathname.
    dir.open(".")?.sync_all().context("sync artifact directory")?;
    Ok(())
}

fn read_file(dir: &Dir, name: &str) -> Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = dir.open_with(name, &options)?;
    ensure!(file.metadata()?.is_file(), "artifact must be a regular file");
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn write_new(dir: &Dir, name: &str, bytes: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).follow(FollowSymlinks::No);
    let mut file = dir.open_with(name, &options)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn store_lock(root: &Dir) -> Result<std::fs::File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).follow(FollowSymlinks::No);
    let file = root.open_with(".control-versions.lock", &options)?.into_std();
    ensure!(file.metadata()?.is_file(), "storage lock must be a regular file");
    // Kernel-owned advisory lock: file can persist, but a killed host never leaves a held lock.
    FileExt::lock_exclusive(&file)?;
    Ok(file)
}

pub fn install(root_path: &Path, request: &InstallRequest) -> Result<InstalledVersion> {
    ensure!(valid_hash(&request.version_id), "version_id must be a 64-character lowercase content hash");
    ensure!(valid_hash(&request.model_sha256) && valid_hash(&request.manifest_sha256), "hashes must be 64 lowercase hexadecimal characters");
    ensure!(!request.approved_by.trim().is_empty(), "approved_by is required");
    ensure!(valid_id(&request.operation_id),"A durable installation operation ID is required");
    let manifest:Value=serde_json::from_str(&request.manifest_json)?;
    validate_approval(&request.approval,&manifest,&request.version_id,&request.model_sha256,&request.manifest_sha256)?;
    ensure!(digest(request.model_json.as_bytes()) == request.model_sha256, "model hash mismatch");
    ensure!(digest(request.manifest_json.as_bytes()) == request.manifest_sha256, "manifest hash mismatch");
    let id = model_id(request.model_json.as_bytes())?;
    validate_manifest(request.manifest_json.as_bytes(), &request.version_id, &request.model_sha256)?;
    let installation = InstalledVersion {
        id: id.clone(),
        model_filename: format!("{PREFIX}{}.json", request.version_id),
        model_sha256: request.model_sha256.clone(),
        control_version_id: request.version_id.clone(),
        manifest_sha256: request.manifest_sha256.clone(),
    };
    let root = open_root(root_path)?;
    let _lock = store_lock(&root)?;
    let slot = child_dir(&root, &id, true)?;
    let versions = child_dir(&slot, VERSIONS, true)?;
    // A successful retry is idempotent only for the very same verified publication.
    match versions.symlink_metadata(&request.version_id) {
        Ok(_) => {
            let (_, _, receipt) = read_version(&versions, &id, &request.version_id)?;
            ensure!(receipt.installation == installation, "immutable version already exists with different content");
            sync_dir(&versions)?;
            return Ok(installation);
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
        Err(e) => return Err(e.into()),
    }
    let staging = format!(".pending-{}", uuid::Uuid::new_v4());
    let staged = child_dir(&versions, &staging, true)?;
    let receipt = Receipt {
        schema_version: 1,
        installation: installation.clone(),
        approved_by: request.approved_by.clone(),
        approval:request.approval.clone(),
        operation_id:request.operation_id.clone(),
        installed_at: chrono::Utc::now().to_rfc3339(),
        authority: "installation_only".to_owned(),
    };
    let publication = (|| -> Result<()> {
        write_new(&staged, "model.json", request.model_json.as_bytes())?;
        write_new(&staged, "manifest.json", request.manifest_json.as_bytes())?;
        write_new(&staged, "receipt.json", &serde_json::to_vec(&receipt)?)?;
        sync_dir(&staged)?;
        // Kernel no-replace publication on Linux, also excluding non-cooperating writers.
        #[cfg(target_os = "linux")]
        rustix::fs::renameat_with(&versions, &staging, &versions, &request.version_id,
            rustix::fs::RenameFlags::NOREPLACE)?;
        #[cfg(not(target_os = "linux"))]
        versions.rename(&staging, &versions, &request.version_id)?;
        sync_dir(&versions)?;
        Ok(())
    })();
    if publication.is_err() {
        // If rename succeeded, keep the complete publication for a verifying retry.
        let _ = versions.remove_dir_all(&staging);
    }
    publication?;
    Ok(installation)
}

pub(super) fn validate_approval(proof:&Value,manifest:&Value,version:&str,model_sha256:&str,manifest_sha256:&str)->Result<()> {
    ensure!(matches!(proof["kind"].as_str(),Some("manual_review"|"bounded_policy")),"Unknown approval authority");
    ensure!(proof["candidate_hash"]==version&&proof["model_sha256"]==model_sha256
        &&proof["manifest_sha256"]==manifest_sha256,"Approval byte identity mismatch");
    ensure!(proof["profile_hash"].as_str().is_some_and(valid_hash)
        &&proof["profile_hash"]==manifest["deployment_profile_hash"],"Approval commissioning profile mismatch");
    ensure!(proof["author_id"].as_i64().is_some_and(|id|id>0)
        &&proof["approver_id"].as_i64().is_some_and(|id|id>0)
        &&proof["author_id"]!=proof["approver_id"]
        &&proof["author_id"]==manifest["author_user_id"],"Independent candidate author/reviewer identity missing");
    let realm=proof["author_identity_realm"].as_str().filter(|realm|!realm.is_empty()).context("Author identity realm missing")?;
    ensure!(proof["approver_identity_realm"]==realm&&manifest["author_identity_realm"]==realm,"Approval identity realms differ");
    ensure!(proof["target"].is_object()&&proof["target"]==manifest["target"]
        &&proof["operating_envelope"].as_str().is_some_and(|value|!value.is_empty())
        &&proof["operating_envelope"]==manifest["operating_envelope"],"Approval equipment scope mismatch");
    let approved:std::collections::BTreeSet<String>=serde_json::from_value(proof["equipment_instance_ids"].clone())?;
    let candidate:std::collections::BTreeSet<String>=serde_json::from_value(manifest["equipment_instance_ids"].clone())?;
    ensure!(!approved.is_empty()&&approved==candidate,"Approval equipment instances mismatch");
    let expiry=chrono::DateTime::parse_from_rfc3339(proof["expires_at"].as_str().context("Approval expiry missing")?)?;
    ensure!(expiry>chrono::Utc::now(),"Approval expired");
    Ok(())
}

fn read_version(versions: &Dir, id: &str, version: &str) -> Result<(Vec<u8>, Vec<u8>, Receipt)> {
    let dir = child_dir(versions, version, false)?;
    let receipt: Receipt = serde_json::from_slice(&read_file(&dir, "receipt.json")?)?;
    let installed = &receipt.installation;
    ensure!(receipt.schema_version == 1 && receipt.authority == "installation_only" && !receipt.approved_by.trim().is_empty(), "invalid installation receipt");
    ensure!(installed.id == id && installed.control_version_id == version
        && installed.model_filename == format!("{PREFIX}{version}.json"), "receipt identity mismatch");
    ensure!(valid_hash(&installed.model_sha256) && valid_hash(&installed.manifest_sha256), "invalid receipt hashes");
    let model = read_file(&dir, "model.json")?;
    let manifest = read_file(&dir, "manifest.json")?;
    ensure!(digest(&model) == installed.model_sha256, "installed model corruption");
    ensure!(digest(&manifest) == installed.manifest_sha256, "installed manifest corruption");
    ensure!(model_id(&model)? == id, "model controller identity mismatch");
    validate_manifest(&manifest, version, &installed.model_sha256)?;
    Ok((model, manifest, receipt))
}

pub(super) fn original_installation(root_path:&Path,id:&str,version:&str,operation_id:&str,profile_hash:&str)->Result<Option<InstalledVersion>> {
    ensure!(valid_id(id)&&valid_hash(version)&&valid_id(operation_id),"Invalid installation identity");
    let root=open_root(root_path)?;let slot=child_dir(&root,id,false)?;let versions=child_dir(&slot,VERSIONS,false)?;
    let (_,_,receipt)=read_version(&versions,id,version)?;
    Ok((receipt.operation_id==operation_id&&receipt.approval["profile_hash"]==profile_hash).then_some(receipt.installation))
}

pub fn load(root_path: &Path, id: &str, filename: &str) -> Result<LoadedModel> {
    load_internal(root_path,id,filename,None)
}

pub(super) fn load_approved(root_path:&Path,id:&str,filename:&str,proof:&Value)->Result<LoadedModel> {
    load_internal(root_path,id,filename,Some(proof))
}

fn load_internal(root_path:&Path,id:&str,filename:&str,proof:Option<&Value>)->Result<LoadedModel> {
    ensure!(valid_id(id) && valid_filename(filename), "invalid controller id or model filename");
    let root = open_root(root_path)?;
    let slot = child_dir(&root, id, false)?;
    let (bytes, relative, version, manifest_json) = if let Some(version) = filename.strip_prefix(PREFIX).and_then(|s| s.strip_suffix(".json")) {
        ensure!(valid_hash(version), "invalid control version filename");
        let versions = child_dir(&slot, VERSIONS, false)?;
        let (bytes, manifest, receipt) = read_version(&versions, id, version)?;
        if let Some(proof)=proof {
            validate_approval(proof,&serde_json::from_slice(&manifest)?,version,&receipt.installation.model_sha256,&receipt.installation.manifest_sha256)?;
        }
        (bytes, PathBuf::from(id).join(VERSIONS).join(version).join("model.json"), Some(version.to_owned()),Some(String::from_utf8(manifest)?))
    } else {
        ensure!(proof.is_none(),"Approved handoff cannot select a mutable manual model");
        let bytes = read_file(&slot, filename)?;
        ensure!(model_id(&bytes)? == id, "model controller identity mismatch");
        (bytes, PathBuf::from(id).join(filename), None,None)
    };
    Ok(LoadedModel { path: root_path.join(relative), model_sha256: digest(&bytes), bytes, control_version_id: version,manifest_json })
}

pub fn list(root_path: &Path) -> Result<Vec<(String, Vec<String>)>> {
    let root = open_root(root_path)?;
    let mut slots = Vec::new();
    for entry in root.entries()? {
        let entry = entry?;
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else { continue };
        if !valid_id(&id) { continue; }
        let Ok(slot) = child_dir(&root, &id, false) else { continue };
        let mut models = Vec::new();
        for entry in slot.entries()? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
            if valid_filename(&name) && !name.starts_with(PREFIX) && load(root_path, &id, &name).is_ok() {
                models.push(name);
            }
        }
        if let Ok(versions) = child_dir(&slot, VERSIONS, false) {
            for entry in versions.entries()? {
                let entry = entry?;
                let Some(version) = entry.file_name().to_str().map(str::to_owned) else { continue };
                if valid_hash(&version) && read_version(&versions, &id, &version).is_ok() {
                    models.push(format!("{PREFIX}{version}.json"));
                }
            }
        }
        models.sort();
        slots.push((id, models));
    }
    slots.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(slots)
}

pub fn upload_manual(root_path: &Path, filename: &str, bytes: &[u8]) -> Result<String> {
    ensure!(valid_filename(filename) && !filename.starts_with(PREFIX), "invalid or reserved model filename");
    let id = model_id(bytes)?;
    let root = open_root(root_path)?;
    let _lock = store_lock(&root)?;
    let slot = child_dir(&root, &id, true)?;
    let stem = filename.strip_suffix(".json").unwrap();
    let filename = format!("{}_{}.json", stem, uuid::Uuid::new_v4());
    ensure!(valid_filename(&filename), "model filename is too long after versioning");
    write_new(&slot, &filename, bytes)?;
    sync_dir(&slot)?;
    Ok(filename)
}

pub fn read_legacy_config(root_path: &Path, id: &str) -> Result<Vec<u8>> {
    ensure!(valid_id(id), "invalid controller id");
    read_file(&open_root(root_path)?, &format!("{id}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> InstallRequest {
        let model_json = "{\n  \"metadata\": {\"name\": \"Kiln_A\"}, \"extra\": \"é\"\n}\n".to_owned();
        let model_sha256 = digest(model_json.as_bytes());
        let version_id = digest(b"fixture-version");
        let profile_hash=digest(b"commissioned-fixture");
        let target=serde_json::json!({"plant":"test","line":"1","area":"cement","group":"mill"});
        let manifest_json = serde_json::json!({"schema_version":1,"id":version_id, "model_sha256":model_sha256,
            "authority":"review_only", "assessment":{"eligible":true,"reasons":[]},
            "model_test":{"passed":true,"used_for_selection":false},"deployment_profile_hash":profile_hash,
            "target":target,"equipment_instance_ids":["fixture-mill"],"operating_envelope":"fixture-only",
            "author_user_id":1,"author_identity_realm":"fixture-realm"}).to_string();
        let manifest_sha256=digest(manifest_json.as_bytes());
        let approval=serde_json::json!({"kind":"manual_review","profile_hash":profile_hash,
            "candidate_hash":version_id,"model_sha256":model_sha256,"manifest_sha256":manifest_sha256,
            "target":target,"equipment_instance_ids":["fixture-mill"],"operating_envelope":"fixture-only",
            "author_id":1,"approver_id":2,"author_identity_realm":"fixture-realm","approver_identity_realm":"fixture-realm",
            "expires_at":(chrono::Utc::now()+chrono::Duration::hours(1)).to_rfc3339()});
        InstallRequest { version_id, model_sha256, manifest_sha256, model_json, manifest_json,
            approved_by:"fixture-approval".into(),operation_id:"fixture-install".into(),approval }
    }

    #[test]
    fn exact_bytes_publication_identity_and_manual_separation() {
        let root = tempfile::tempdir().unwrap();
        let req = request();
        let installed = install(root.path(), &req).unwrap();
        let loaded = load(root.path(), &installed.id, &installed.model_filename).unwrap();
        assert_eq!(loaded.bytes, req.model_json.as_bytes());
        assert_eq!(loaded.model_sha256, req.model_sha256);
        assert_eq!(loaded.control_version_id.as_deref(), Some(req.version_id.as_str()));
        let folder = root.path().join("Kiln_A/.control-versions").join(&req.version_id);
        assert_eq!(std::fs::read(folder.join("manifest.json")).unwrap(), req.manifest_json.as_bytes());
        let receipt: Receipt = serde_json::from_slice(&std::fs::read(folder.join("receipt.json")).unwrap()).unwrap();
        assert_eq!(receipt.approved_by, req.approved_by);
        assert_eq!(receipt.authority, "installation_only");
        assert_eq!(list(root.path()).unwrap()[0].1, vec![installed.model_filename]);
        let name = upload_manual(root.path(), "manual.json", req.model_json.as_bytes()).unwrap();
        let manual = load(root.path(), "Kiln_A", &name).unwrap();
        assert_eq!(manual.control_version_id, None);
        assert_eq!(manual.model_sha256, req.model_sha256);
    }

    #[test]
    fn rejects_wrong_hash_and_unqualified_manifest() {
        let root = tempfile::tempdir().unwrap();
        let mut req = request();
        req.model_sha256 = "0".repeat(64);
        assert!(install(root.path(), &req).is_err());
        req = request();
        req.manifest_sha256 = "A".repeat(64);
        assert!(install(root.path(), &req).is_err());
        for (pointer, value) in [("/authority", Value::String("control".into())), ("/assessment/eligible", Value::Bool(false)),
            ("/model_test/passed", Value::Bool(false)), ("/model_test/used_for_selection", Value::Bool(true)),
            ("/id", Value::String("other".into())), ("/model_sha256", Value::String("0".repeat(64))),
            ("/assessment/reasons", serde_json::json!(["not qualified"]))] {
            let mut req = request();
            let mut manifest: Value = serde_json::from_str(&req.manifest_json).unwrap();
            *manifest.pointer_mut(pointer).unwrap() = value;
            req.manifest_json = manifest.to_string();
            req.manifest_sha256 = digest(req.manifest_json.as_bytes());
            assert!(install(root.path(), &req).is_err(), "accepted {pointer}");
        }
        assert!(list(root.path()).unwrap().is_empty());
    }

    #[test]
    fn immutable_retry_and_corruption_rejected_before_launch() {
        let root = tempfile::tempdir().unwrap();
        let req = request();
        let installed = install(root.path(), &req).unwrap();
        assert_eq!(install(root.path(), &req).unwrap(), installed);
        let mut changed = request();
        changed.manifest_json.push('\n');
        changed.manifest_sha256 = digest(changed.manifest_json.as_bytes());
        assert!(install(root.path(), &changed).is_err());
        let folder = root.path().join("Kiln_A/.control-versions").join(&req.version_id);
        for name in ["model.json", "manifest.json", "receipt.json"] {
            let path = folder.join(name);
            let original = std::fs::read(&path).unwrap();
            std::fs::write(&path, b"corrupt").unwrap();
            assert!(load(root.path(), "Kiln_A", &installed.model_filename).is_err());
            assert!(install(root.path(), &req).is_err());
            std::fs::write(path, original).unwrap();
        }
    }

    #[test]
    fn interrupted_staging_does_not_publish_or_block_retry() {
        let root = tempfile::tempdir().unwrap();
        let staging = root.path().join("Kiln_A/.control-versions/.pending-abandoned");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("model.json"), b"partial").unwrap();
        assert!(list(root.path()).unwrap()[0].1.is_empty());
        let req = request();
        let installed = install(root.path(), &req).unwrap();
        assert_eq!(list(root.path()).unwrap()[0].1, vec![installed.model_filename]);
    }

    #[test]
    fn rejects_traversal_and_namespace_sanitization() {
        let root = tempfile::tempdir().unwrap();
        for id in ["../escape", "", ".", "a/b", "a\\b", "Kiln A"] {
            assert!(load(root.path(), id, "model.json").is_err());
            let mut req = request();
            req.version_id = id.into();
            assert!(install(root.path(), &req).is_err());
            let model = serde_json::json!({"metadata":{"name":id}}).to_string();
            assert!(upload_manual(root.path(), "model.json", model.as_bytes()).is_err());
        }
        for filename in ["../model.json", "/model.json", "a/b.json", "a\\b.json", "control-version-v.json"] {
            assert!(upload_manual(root.path(), filename, request().model_json.as_bytes()).is_err());
        }
        for version in ["version_1".to_owned(), "A".repeat(64), "a".repeat(63), "a".repeat(65)] {
            let mut req = request();
            req.version_id = version;
            assert!(install(root.path(), &req).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_slots_versions_files_and_lock() {
        use std::os::unix::fs::symlink;
        let req = request();
        for target in ["Kiln_A".to_owned(), "Kiln_A/.control-versions".to_owned(), format!("Kiln_A/.control-versions/{}", req.version_id), ".control-versions.lock".to_owned()] {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let link = root.path().join(&target);
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            symlink(outside.path(), &link).unwrap();
            assert!(install(root.path(), &req).is_err(), "accepted linked {target}");
        }
        for name in ["model.json", "manifest.json", "receipt.json"] {
            let root = tempfile::tempdir().unwrap();
            let installed = install(root.path(), &req).unwrap();
            let path = root.path().join("Kiln_A/.control-versions").join(&req.version_id).join(name);
            let external = root.path().join("outside.json");
            std::fs::rename(&path, &external).unwrap();
            symlink(&external, &path).unwrap();
            assert!(load(root.path(), "Kiln_A", &installed.model_filename).is_err());
        }
    }

    #[test]
    fn concurrent_install_is_idempotent_and_lock_releases_on_drop() {
        let root = tempfile::tempdir().unwrap();
        let dir = open_root(root.path()).unwrap();
        let held = store_lock(&dir).unwrap();
        let mut options = OpenOptions::new();
        options.read(true).write(true).follow(FollowSymlinks::No);
        let contender = dir.open_with(".control-versions.lock", &options).unwrap().into_std();
        assert!(FileExt::try_lock_exclusive(&contender).is_err());
        drop(held);
        FileExt::try_lock_exclusive(&contender).unwrap();
        drop(contender);
        let path = root.path().to_owned();
        let first = std::thread::spawn(move || install(&path, &request()).unwrap());
        let second = install(root.path(), &request()).unwrap();
        assert_eq!(first.join().unwrap(), second);
        assert_eq!(list(root.path()).unwrap()[0].1, vec![second.model_filename]);
    }

    #[test]
    fn incomplete_final_publication_is_not_overwritten_or_listed() {
        let root = tempfile::tempdir().unwrap();
        let req = request();
        let folder = root.path().join("Kiln_A/.control-versions").join(&req.version_id);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("model.json"), b"partial").unwrap();
        assert!(install(root.path(), &req).is_err());
        assert_eq!(std::fs::read(folder.join("model.json")).unwrap(), b"partial");
        assert!(list(root.path()).unwrap()[0].1.is_empty());
    }

    #[test]
    fn approval_requires_independent_same_realm_exact_candidate() {
        for (field,value) in [
            ("approver_id",serde_json::json!(1)),
            ("approver_identity_realm",serde_json::json!("another-database")),
            ("model_sha256",serde_json::json!("0".repeat(64))),
            ("profile_hash",serde_json::json!("0".repeat(64))),
            ("equipment_instance_ids",serde_json::json!(["another-mill"])),
            ("expires_at",serde_json::json!("2000-01-01T00:00:00Z")),
        ] {
            let root=tempfile::tempdir().unwrap();let mut req=request();
            req.approval[field]=value;
            assert!(install(root.path(),&req).is_err(),"accepted invalid {field}");
            assert!(!root.path().join("Kiln_A").exists(),"rejected approval published artifact");
        }
    }

    #[test]
    fn original_installation_receipt_cannot_confirm_another_operation() {
        let root=tempfile::tempdir().unwrap();let req=request();
        let installed=install(root.path(),&req).unwrap();
        let profile=req.approval["profile_hash"].as_str().unwrap();
        assert!(original_installation(root.path(),&installed.id,&req.version_id,&req.operation_id,profile).unwrap().is_some());
        assert!(original_installation(root.path(),&installed.id,&req.version_id,"another-operation",profile).unwrap().is_none());
        assert!(original_installation(root.path(),&installed.id,&req.version_id,&req.operation_id,&"0".repeat(64)).unwrap().is_none());
    }
}
