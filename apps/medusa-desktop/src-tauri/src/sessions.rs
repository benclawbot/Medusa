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
pub struct DesktopSessionDetail {
    pub summary: DesktopSessionSummary,
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
pub fn runtime_read_session(repo: String, session_id: String) -> Result<DesktopSessionDetail, String> {
    let repo = canonical_repo(&repo)?;
    let value = read_session_value(&repo, &session_id)?;
    let summary = summary_from_value(&value)
        .ok_or_else(|| format!("session {session_id} is missing required metadata"))?;
    let messages = value
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(message_from_value)
        .collect();
    Ok(DesktopSessionDetail { summary, messages })
}

fn canonical_repo(repo: &str) -> Result<PathBuf, String> {
    let repo = fs::canonicalize(Path::new(repo))
        .map_err(|error| format!("cannot open {repo}: {error}"))?;
    if !repo.is_dir() {
        return Err(format!("{} is not a directory", repo.display()));
    }
    Ok(repo)
}

fn read_session_value(repo: &Path, session_id: &str) -> Result<Value, String> {
    if session_id.is_empty()
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err("invalid session id".to_owned());
    }
    for root in [repo.join(".medusa/sessions"), fallback_session_root(repo)] {
        let path = root.join(format!("{session_id}.json"));
        if path.is_file() {
            return serde_json::from_slice(
                &fs::read(&path).map_err(|error| format!("cannot read {}: {error}", path.display()))?,
            )
            .map_err(|error| format!("cannot parse {}: {error}", path.display()));
        }
    }
    Err(format!("session {session_id} was not found for {}", repo.display()))
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

fn message_from_value(value: &Value) -> Option<DesktopSessionMessage> {
    let role = value.get("role")?.as_str()?.to_owned();
    let text = value
        .get("content")?
        .as_array()?
        .iter()
        .filter_map(block_text)
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then_some(DesktopSessionMessage { role, text })
}

fn block_text(value: &Value) -> Option<String> {
    match value.get("type")?.as_str()? {
        "text" => value.get("text")?.as_str().map(str::to_owned),
        "image" => Some("[Image attachment]".to_owned()),
        "tool_use" => Some(format!(
            "Tool: {}",
            value.get("name").and_then(Value::as_str).unwrap_or("unknown")
        )),
        "tool_result" => value.get("content")?.as_str().map(str::to_owned),
        _ => None,
    }
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
