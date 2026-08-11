use std::{fs, process::Command};

use medusa_core::MedusaResult;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::format_description::well_known::Rfc3339;

use super::{AgentSession, completed_learning};

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
    repository_revision: Option<String>,
    outcome: String,
    events: Vec<RecallEvent>,
}

pub(super) fn persist_completed_session(session: &AgentSession) -> MedusaResult<()> {
    let policy = completed_learning::policy_for(&session.repo)?;
    if !policy.capture_enabled() || !completed_learning::authoritative_success(session) {
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
        repository_fingerprint: repository_fingerprint(&session.repo),
        repository_revision: git_output(&session.repo, &["rev-parse", "HEAD"]),
        outcome: "authoritatively_verified".to_owned(),
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

fn repository_fingerprint(repo: &std::path::Path) -> String {
    let identity = git_output(repo, &["remote", "get-url", "origin"])
        .map(|origin| {
            origin
                .trim()
                .trim_end_matches('/')
                .trim_end_matches(".git")
                .to_ascii_lowercase()
        })
        .or_else(|| {
            git_output(repo, &["rev-list", "--max-parents=0", "HEAD"]).map(|roots| {
                let mut roots = roots.lines().map(str::trim).collect::<Vec<_>>();
                roots.sort_unstable();
                format!("git-roots:{}", roots.join(","))
            })
        })
        .unwrap_or_else(|| "unresolved-repository".to_owned());
    hex::encode(Sha256::digest(identity.as_bytes()))
}

fn git_output(repo: &std::path::Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(repo)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn find_string(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(Value::String(value)) = map.get(*key)
                    && !value.trim().is_empty()
                {
                    return Some(value.clone());
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
    use medusa_protocol::{Actor, EventPayload};
    use serde_json::json;
    use time::OffsetDateTime;

    use crate::evidence::append_event;

    use super::*;

    fn verified_session(directory: &std::path::Path) -> AgentSession {
        let mut session = AgentSession {
            id: SessionId::new(),
            objective: "repair the update command".to_owned(),
            repo: PathBuf::from(directory),
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
            approval_grants: Vec::new(),
            approval_receipts: Vec::new(),
            rollback_receipts: Vec::new(),
            world_model: None,
        };
        append_event(
            &mut session,
            Actor::System("test".to_owned()),
            EventPayload::VerificationCompleted {
                passed: true,
                evidence: vec!["verified".to_owned()],
            },
        )
        .expect("verification");
        append_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::SessionCompleted {
                report_ref: "report".to_owned(),
            },
        )
        .expect("completion");
        session
    }

    #[test]
    fn completed_authoritative_session_is_written_to_recall_inbox() {
        let directory = tempfile::tempdir().expect("tempdir");
        let session = verified_session(directory.path());

        persist_completed_session(&session).expect("persist recall");
        let path = directory
            .path()
            .join(".medusa/session-recall-inbox")
            .join(format!("{}.json", session.id));
        let value: Value = serde_json::from_slice(&fs::read(path).expect("inbox record"))
            .expect("valid recall record");
        assert_eq!(value["session_id"], session.id.to_string());
        assert_eq!(value["outcome"], "authoritatively_verified");
    }

    #[test]
    fn capture_disabled_leaves_no_recall_content() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path().join(".medusa/learning-review");
        fs::create_dir_all(&root).expect("privacy root");
        fs::write(
            root.join("state.json"),
            serde_json::to_vec_pretty(&json!({
                "schema_version": 1,
                "revision": 1,
                "privacy": {
                    "capture_enabled": false,
                    "user_persistence_enabled": false,
                    "cross_repository_reuse_enabled": false,
                    "telemetry_enabled": false,
                    "automatic_proposals_enabled": false
                },
                "items": [],
                "audit_head": "0000000000000000000000000000000000000000000000000000000000000000"
            }))
            .expect("privacy json"),
        )
        .expect("privacy");
        let mut session = verified_session(directory.path());
        session.objective = "SEEDED_PRIVATE_CONTENT".to_owned();
        persist_completed_session(&session).expect("privacy block");
        assert!(!directory.path().join(".medusa/session-recall-inbox").exists());
    }
}
