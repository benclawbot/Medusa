use std::{
    fs,
    io::Write,
    path::{Component, Path},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::skill_dependencies::resolve_project_skill;

pub const LOCK_FILE: &str = "dependency-lock.json";
const MANIFEST_FILE: &str = "dependencies.json";
const SKILL_FILE: &str = "SKILL.md";
const EMPTY_MANIFEST: &[u8] = b"{\"schema_version\":1,\"requires\":[]}";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LockedSkill {
    pub name: String,
    pub skill_sha256: String,
    pub manifest_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyLockReceipt {
    pub schema_version: u64,
    pub selected: String,
    pub order: Vec<String>,
    pub skills: Vec<LockedSkill>,
    pub graph_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LockVerification {
    pub selected: String,
    pub graph_sha256: String,
    pub locked: bool,
    pub valid: bool,
}

pub fn compute_dependency_lock(
    root: &Path,
    selected: &str,
) -> Result<DependencyLockReceipt, String> {
    validate_name(selected)?;
    let resolved = resolve_project_skill(root, selected, usize::MAX)?;
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| format!("resolve approved skill root {}: {error}", root.display()))?;
    let mut skills = Vec::with_capacity(resolved.order.len());
    for name in &resolved.order {
        validate_name(name)?;
        let directory = confined_directory(&canonical_root, name)?;
        let skill = confined_file(
            &canonical_root,
            &directory.join(SKILL_FILE),
            name,
            "skill file",
        )?;
        let skill_bytes =
            fs::read(&skill).map_err(|error| format!("read {}: {error}", skill.display()))?;
        let manifest = directory.join(MANIFEST_FILE);
        let manifest_bytes = if manifest.exists() {
            let manifest = confined_file(&canonical_root, &manifest, name, "dependency manifest")?;
            let bytes = fs::read(&manifest)
                .map_err(|error| format!("read {}: {error}", manifest.display()))?;
            canonical_manifest(&bytes, &manifest)?
        } else {
            EMPTY_MANIFEST.to_vec()
        };
        skills.push(LockedSkill {
            name: name.clone(),
            skill_sha256: sha256(&skill_bytes),
            manifest_sha256: sha256(&manifest_bytes),
        });
    }
    let canonical = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "selected": selected,
        "order": resolved.order,
        "skills": skills,
    }))
    .map_err(|error| format!("serialize dependency lock payload: {error}"))?;
    Ok(DependencyLockReceipt {
        schema_version: 1,
        selected: selected.to_owned(),
        order: resolved.order,
        skills,
        graph_sha256: sha256(&canonical),
    })
}

pub fn write_dependency_lock(root: &Path, selected: &str) -> Result<DependencyLockReceipt, String> {
    let receipt = compute_dependency_lock(root, selected)?;
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| format!("resolve approved skill root {}: {error}", root.display()))?;
    let directory = confined_directory(&canonical_root, selected)?;
    let destination = directory.join(LOCK_FILE);
    if destination.exists() {
        let metadata = fs::symlink_metadata(&destination)
            .map_err(|error| format!("inspect {}: {error}", destination.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!(
                "dependency lock for `{selected}` must be a regular file"
            ));
        }
    }
    let bytes = serde_json::to_vec_pretty(&receipt)
        .map_err(|error| format!("serialize dependency lock: {error}"))?;
    let temporary = directory.join(format!(".{LOCK_FILE}.tmp"));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| format!("create {}: {error}", temporary.display()))?;
        file.write_all(&bytes)
            .and_then(|_| file.write_all(b"\n"))
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("write {}: {error}", temporary.display()))?;
        fs::rename(&temporary, &destination)
            .map_err(|error| format!("replace dependency lock {}: {error}", destination.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    Ok(receipt)
}

pub fn verify_dependency_lock(root: &Path, selected: &str) -> Result<LockVerification, String> {
    verify_dependency_lock_required(root, selected, true)
}

pub fn verify_dependency_lock_if_present(
    root: &Path,
    selected: &str,
) -> Result<Option<LockVerification>, String> {
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| format!("resolve approved skill root {}: {error}", root.display()))?;
    let directory = confined_directory(&canonical_root, selected)?;
    if !directory.join(LOCK_FILE).exists() {
        return Ok(None);
    }
    verify_dependency_lock_required(root, selected, true).map(Some)
}

fn verify_dependency_lock_required(
    root: &Path,
    selected: &str,
    required: bool,
) -> Result<LockVerification, String> {
    validate_name(selected)?;
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| format!("resolve approved skill root {}: {error}", root.display()))?;
    let directory = confined_directory(&canonical_root, selected)?;
    let path = directory.join(LOCK_FILE);
    if !path.exists() {
        if required {
            return Err(format!("dependency lock is missing for `{selected}`"));
        }
        return Ok(LockVerification {
            selected: selected.to_owned(),
            graph_sha256: String::new(),
            locked: false,
            valid: true,
        });
    }
    let path = confined_file(&canonical_root, &path, selected, "dependency lock")?;
    let stored: DependencyLockReceipt = serde_json::from_slice(
        &fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?,
    )
    .map_err(|error| format!("parse {}: {error}", path.display()))?;
    if stored.schema_version != 1 {
        return Err(format!("{} requires schema_version 1", path.display()));
    }
    if stored.selected != selected {
        return Err(format!(
            "dependency lock for `{selected}` selects `{}`",
            stored.selected
        ));
    }
    let current = compute_dependency_lock(root, selected)?;
    if stored != current {
        return Err(format!(
            "dependency lock for `{selected}` is stale: expected {}, found {}",
            current.graph_sha256, stored.graph_sha256
        ));
    }
    Ok(LockVerification {
        selected: selected.to_owned(),
        graph_sha256: current.graph_sha256,
        locked: true,
        valid: true,
    })
}

fn canonical_manifest(bytes: &[u8], path: &Path) -> Result<Vec<u8>, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    serde_json::to_vec(&value).map_err(|error| format!("canonicalize {}: {error}", path.display()))
}

fn confined_directory(root: &Path, name: &str) -> Result<std::path::PathBuf, String> {
    validate_name(name)?;
    let directory = root.join(name);
    let canonical = fs::canonicalize(&directory)
        .map_err(|error| format!("resolve {}: {error}", directory.display()))?;
    if !canonical.starts_with(root) {
        return Err(format!("skill `{name}` escapes approved skill root"));
    }
    Ok(canonical)
}

fn confined_file(
    root: &Path,
    path: &Path,
    name: &str,
    label: &str,
) -> Result<std::path::PathBuf, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("inspect {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("{label} for `{name}` must be a regular file"));
    }
    let canonical =
        fs::canonicalize(path).map_err(|error| format!("resolve {}: {error}", path.display()))?;
    if !canonical.starts_with(root) {
        return Err(format!("{label} for `{name}` escapes approved skill root"));
    }
    Ok(canonical)
}

fn validate_name(name: &str) -> Result<(), String> {
    let path = Path::new(name);
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', '@'])
        || name.contains("..")
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        return Err(format!("invalid skill name `{name}`"));
    }
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
