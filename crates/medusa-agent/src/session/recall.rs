use std::fs;

use medusa_core::MedusaResult;
use serde::Serialize;
use serde_json::Value;
use time::format_description::well_known::Rfc3339;

use super::AgentSession;

#[derive(Serialize)]
struct RecallEvent {
    ordinal: usize,
    kind: String,
    tool: Option<String>,
    success: Option<bool>,
    text: String,
}

#[derive(Serialize)]
struct RecallRecord {
    session_id: String,
    parent_session_id: Option<String>,
    created_at: String,
    repository_fingerprint: String,
    outcome: String,
    events: Vec<RecallEvent>,
}

pub(super) fn persist_completed_session(session: &AgentSession) -> MedusaResult<()> {
    if !session.completed {
        return Ok(());
    }

    let mut events = Vec::with_capacity(session.messages.len() + session.events.len());
    for (ordinal, message) in session.messages.iter().enumerate() {
        let value = serde_json::to_value(message)?;
        events.push(RecallEvent {
            ordinal,
            kind: "message".to_owned(),
            tool: find_string(&value, &["tool", "name"]),
            success: find_bool(&value, &["success", "ok"]),
            text: serde_json::to_string(message)?,
        });
    }

    let offset = events.len();
    for (index, envelope) in session.events.iter().enumerate() {
        let value = serde_json::to_value(envelope)?;
        events.push(RecallEvent {
            ordinal: offset + index,
            kind: "event".to_owned(),
            tool: find_string(&value, &["tool", "name"]),
            success: find_bool(&value, &["success", "ok"]),
            text: serde_json::to_string(envelope)?,
        });
    }

    if events.is_empty() {
        events.push(RecallEvent {
            ordinal: 0,
            kind: "objective".to_owned(),
            tool: None,
            success: Some(true),
            text: session.objective.clone(),
        });
    }

    let record = RecallRecord {
        session_id: session.id.to_string(),
        parent_session_id: None,
        created_at: session.created_at.format(&Rfc3339).map_err(|error| {
            medusa_core::MedusaError::new(
                medusa_core::ErrorCode::PersistenceFailed,
                medusa_core::ErrorCategory::Persistence,
                format!("cannot format session recall timestamp: {error}"),
            )
        })?,
        repository_fingerprint: format!("path:{}", session.repo.to_string_lossy()),
        outcome: "success".to_owned(),
        events,
    };

    let inbox = session.repo.join(".medusa/session-recall-inbox");
    fs::create_dir_all(&inbox)?;
    let path = inbox.join(format!("{}.json", session.id));
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(&record)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn find_string(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(Value::String(value)) = map.get(*key) {
                    if !value.trim().is_empty() {
                        return Some(value.clone());
                    }
                }
            }
            map.values().find_map(|value| find_string(value, keys))
        }
        Value::Array(values) => values.iter().find_map(|value| find_string(value, keys)),
        _ => None,
    }
}

fn find_bool(value: &Value, keys: &[&str]) -> Option<bool> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(Value::Bool(value)) = map.get(*key) {
                    return Some(*value);
                }
            }
            map.values().find_map(|value| find_bool(value, keys))
        }
        Value::Array(values) => values.iter().find_map(|value| find_bool(value, keys)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use medusa_core::SessionId;
    use time::OffsetDateTime;

    use super::*;

    #[test]
    fn completed_session_is_written_to_recall_inbox() {
        let directory = tempfile::tempdir().expect("tempdir");
        let session = AgentSession {
            id: SessionId::new(),
            objective: "repair the update command".to_owned(),
            repo: PathBuf::from(directory.path()),
            created_at: OffsetDateTime::now_utc(),
            updated_at: OffsetDateTime::now_utc(),
            completed: true,
            turn: 1,
            plan: Vec::new(),
            pending_question: None,
            messages: Vec::new(),
            events: Vec::new(),
            evidence: vec!["verified".to_owned()],
            tool_artifacts: Vec::new(),
        };

        persist_completed_session(&session).expect("persist recall");
        let path = directory
            .path()
            .join(".medusa/session-recall-inbox")
            .join(format!("{}.json", session.id));
        let value: Value = serde_json::from_slice(&fs::read(path).expect("inbox record"))
            .expect("valid recall record");
        assert_eq!(value["session_id"], session.id.to_string());
        assert_eq!(value["events"][0]["text"], "repair the update command");
    }
}
