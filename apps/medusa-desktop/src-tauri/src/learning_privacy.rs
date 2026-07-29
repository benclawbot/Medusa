use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LearningPrivacySettings {
    #[serde(default = "default_true")]
    pub capture_enabled: bool,
    #[serde(default = "default_true")]
    pub repository_persistence: bool,
    #[serde(default)]
    pub cross_repository_reuse: bool,
    #[serde(default)]
    pub telemetry_enabled: bool,
    #[serde(default = "default_true")]
    pub automatic_proposals: bool,
}

impl Default for LearningPrivacySettings {
    fn default() -> Self {
        Self {
            capture_enabled: true,
            repository_persistence: true,
            cross_repository_reuse: false,
            telemetry_enabled: false,
            automatic_proposals: true,
        }
    }
}

const fn default_true() -> bool {
    true
}

#[tauri::command]
pub fn runtime_learning_privacy(repo: String) -> Result<LearningPrivacySettings, String> {
    let repo = canonical_repo(&repo)?;
    read_privacy(&repo)
}

#[tauri::command]
pub fn runtime_update_learning_privacy(
    repo: String,
    settings: LearningPrivacySettings,
) -> Result<LearningPrivacySettings, String> {
    let repo = canonical_repo(&repo)?;
    write_json_atomic(&privacy_path(&repo), &settings)?;
    append_audit(
        &repo,
        "privacy.updated",
        None,
        serde_json::to_value(&settings).unwrap_or(Value::Null),
    )?;
    Ok(settings)
}

#[tauri::command]
pub fn runtime_redact_improvement(repo: String, id: String) -> Result<(), String> {
    let repo = canonical_repo(&repo)?;
    let path = improvements_path(&repo);
    let mut records = read_records(&path)?;
    let item = records
        .iter_mut()
        .find(|item| item.get("id").and_then(Value::as_str) == Some(id.as_str()))
        .ok_or("improvement not found")?;
    let object = item
        .as_object_mut()
        .ok_or("improvement record is not an object")?;
    object.insert("evidence".into(), Value::Array(Vec::new()));
    object.insert("sourceSessions".into(), Value::Array(Vec::new()));
    object.insert("problem".into(), Value::String("Redacted by user".into()));
    object.insert("approval".into(), Value::Null);
    object.insert("status".into(), Value::String("pending".into()));
    let revision = object
        .get("revision")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .saturating_add(1);
    object.insert("revision".into(), Value::from(revision));
    object.insert("updatedAt".into(), Value::String(now()?));
    write_json_atomic(&path, &records)?;
    append_audit(&repo, "improvement.redacted", Some(&id), Value::Null)
}

#[tauri::command]
pub fn runtime_delete_improvement(repo: String, id: String) -> Result<(), String> {
    let repo = canonical_repo(&repo)?;
    let path = improvements_path(&repo);
    let mut records = read_records(&path)?;
    let before = records.len();
    records.retain(|item| item.get("id").and_then(Value::as_str) != Some(id.as_str()));
    if records.len() == before {
        return Err("improvement not found".into());
    }
    write_json_atomic(&path, &records)?;
    append_audit(&repo, "improvement.deleted", Some(&id), Value::Null)
}

#[tauri::command]
pub fn runtime_export_learning_audit(repo: String) -> Result<String, String> {
    let repo = canonical_repo(&repo)?;
    let source = audit_path(&repo);
    let destination = repo.join(".medusa/engineering/learning-audit-export.jsonl");
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(&destination, fs::read(source).unwrap_or_default())
        .map_err(|error| error.to_string())?;
    Ok(destination.to_string_lossy().into_owned())
}

fn canonical_repo(repo: &str) -> Result<PathBuf, String> {
    let path = fs::canonicalize(repo).map_err(|error| format!("cannot open {repo}: {error}"))?;
    path.is_dir()
        .then_some(path)
        .ok_or_else(|| "repository is not a directory".into())
}

fn privacy_path(repo: &Path) -> PathBuf {
    repo.join(".medusa/engineering/privacy.json")
}

fn audit_path(repo: &Path) -> PathBuf {
    repo.join(".medusa/engineering/learning-audit.jsonl")
}

fn improvements_path(repo: &Path) -> PathBuf {
    repo.join(".medusa/engineering/improvements.json")
}

fn read_privacy(repo: &Path) -> Result<LearningPrivacySettings, String> {
    let path = privacy_path(repo);
    if !path.exists() {
        return Ok(LearningPrivacySettings::default());
    }
    serde_json::from_slice(&fs::read(path).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
}

fn read_records(path: &Path) -> Result<Vec<Value>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    serde_json::from_slice(&fs::read(path).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    fs::rename(temporary, path).map_err(|error| error.to_string())
}

fn append_audit(
    repo: &Path,
    event: &str,
    item_id: Option<&str>,
    payload: Value,
) -> Result<(), String> {
    let path = audit_path(repo);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let previous_hash = fs::read_to_string(&path)
        .ok()
        .and_then(|text| text.lines().last().map(str::to_owned))
        .and_then(|line| serde_json::from_str::<Value>(&line).ok())
        .and_then(|value| {
            value
                .get("hash")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default();
    let mut record = serde_json::json!({
        "timestamp": now()?,
        "event": event,
        "itemId": item_id,
        "payload": payload,
        "previousHash": previous_hash,
    });
    let hash = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&record).map_err(|error| error.to_string())?)
    );
    record["hash"] = Value::String(hash);
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    serde_json::to_writer(&mut file, &record).map_err(|error| error.to_string())?;
    file.write_all(b"\n").map_err(|error| error.to_string())
}

fn now() -> Result<String, String> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| error.to_string())
}
