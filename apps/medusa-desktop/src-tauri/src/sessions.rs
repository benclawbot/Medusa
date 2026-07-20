use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSessionSummary {
    pub id: String,
    pub objective: String,
    pub created_at: String,
    pub updated_at: String,
    pub completed: bool,
    pub waiting_for_user: bool,
    pub turn: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSessionMessage {
    pub role: String,
    pub text: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSessionTranscript {
    pub id: String,
    pub objective: String,
    pub messages: Vec<DesktopSessionMessage>,
}

#[tauri::command]
pub fn runtime_list_sessions(repo: String) -> Result<Vec<DesktopSessionSummary>, String> {
    let repo = canonical_repo(&repo)?;
    let mut sessions = BTreeMap::new();
    collect_sessions(&repo.join(".medusa/sessions"), &mut sessions)?;
    collect_sessions(&fallback_session_root(&repo), &mut sessions)?;
    let mut sessions = sessions.into_values().collect::<Vec<_>>();
    sessions.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(sessions)
}

#[tauri::command]
pub fn runtime_read_session(
    repo: String,
    session_id: String,
) -> Result<DesktopSessionTranscript, String> {
    let repo = canonical_repo(&repo)?;
    validate_session_id(&session_id)?;
    let path = session_path(&repo, &session_id)
        .ok_or_else(|| format!("session {session_id} was not found for {}", repo.display()))?;
    let value: Value = serde_json::from_slice(
        &fs::read(&path).map_err(|error| format!("cannot read {}: {error}", path.display()))?,
    )
    .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{} has no session id", path.display()))?
        .to_owned();
    if id != session_id {
        return Err(format!("session id mismatch in {}", path.display()));
    }
    let objective = value
        .get("objective")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let messages = value
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(message_from_value)
        .collect();
    Ok(DesktopSessionTranscript {
        id,
        objective,
        messages,
    })
}

fn canonical_repo(repo: &str) -> Result<PathBuf, String> {
    let repo = fs::canonicalize(Path::new(repo))
        .map_err(|error| format!("cannot open {repo}: {error}"))?;
    if !repo.is_dir() {
        return Err(format!("{} is not a directory", repo.display()));
    }
    Ok(repo)
}

fn validate_session_id(session_id: &str) -> Result<(), String> {
    if session_id.is_empty()
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err("invalid session id".to_owned());
    }
    Ok(())
}

fn session_path(repo: &Path, session_id: &str) -> Option<PathBuf> {
    let filename = format!("{session_id}.json");
    [
        repo.join(".medusa/sessions").join(&filename),
        fallback_session_root(repo).join(filename),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

fn message_from_value(value: &Value) -> Option<DesktopSessionMessage> {
    let role = value.get("role")?.as_str()?.to_owned();
    let mut text = Vec::new();
    collect_text(value.get("content")?, &mut text);
    let text = text
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(DesktopSessionMessage { role, text })
}

fn collect_text(value: &Value, output: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_text(item, output);
            }
        }
        Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(Value::as_str) {
                output.push(text.to_owned());
                return;
            }
            for nested in map.values() {
                collect_text(nested, output);
            }
        }
        _ => {}
    }
}

fn collect_sessions(
    root: &Path,
    sessions: &mut BTreeMap<String, DesktopSessionSummary>,
) -> Result<(), String> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("cannot read {}: {error}", root.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read session entry: {error}"))?;
        let path = entry.path();
        if !entry
            .file_type()
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?
            .is_file()
            || path.extension().and_then(|value| value.to_str()) != Some("json")
        {
            continue;
        }
        let value: Value = serde_json::from_slice(
            &fs::read(&path).map_err(|error| format!("cannot read {}: {error}", path.display()))?,
        )
        .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
        let Some(summary) = summary_from_value(&value) else {
            continue;
        };
        sessions.entry(summary.id.clone()).or_insert(summary);
    }
    Ok(())
}

fn summary_from_value(value: &Value) -> Option<DesktopSessionSummary> {
    Some(DesktopSessionSummary {
        id: value.get("id")?.as_str()?.to_owned(),
        objective: value.get("objective")?.as_str()?.to_owned(),
        created_at: value.get("created_at")?.as_str()?.to_owned(),
        updated_at: value.get("updated_at")?.as_str()?.to_owned(),
        completed: value.get("completed")?.as_bool()?,
        waiting_for_user: value
            .get("pending_question")
            .is_some_and(|question| !question.is_null()),
        turn: u32::try_from(value.get("turn")?.as_u64()?).ok()?,
    })
}

fn fallback_session_root(repo: &Path) -> PathBuf {
    let root = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir);
    root.join("Medusa/sessions").join(repository_key(repo))
}

fn repository_key(repo: &Path) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in repo.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}
