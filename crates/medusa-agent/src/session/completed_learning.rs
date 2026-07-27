use std::{
    fs,
    path::{Path, PathBuf},
};

use medusa_core::MedusaResult;
use serde_json::{Value, json};

use super::{AgentSession, lessons, skill_drafts, skill_outcomes, skill_probation};

const MIN_PROBATION_CONFIDENCE_MILLI: u64 = 750;

pub(super) fn process(session: &AgentSession) -> MedusaResult<()> {
    if !session.completed {
        return Ok(());
    }

    let marker = processed_marker(session);
    if marker.is_file() {
        return Ok(());
    }

    if let Some(proposal_path) = lessons::extract_completed_session(session)? {
        let canonical_path = admit_to_canonical_memory(session, &proposal_path)?;
        let value: Value = serde_json::from_slice(&fs::read(&canonical_path)?)?;
        if value["lifecycle"]["status"] == "probation" {
            skill_drafts::create_from_lesson(&canonical_path)?;
        }
    }

    skill_outcomes::record_completed_session(session)?;
    skill_probation::refresh(&session.repo)?;
    write_json_atomic(
        &marker,
        &json!({
            "session_id": session.id.to_string(),
            "repository": session.repo.to_string_lossy(),
            "completed": true,
            "evidence_count": session.evidence.len(),
            "verification_result": verification_result(session),
        }),
    )
}

fn admit_to_canonical_memory(
    session: &AgentSession,
    proposal_path: &Path,
) -> MedusaResult<PathBuf> {
    let mut proposal: Value = serde_json::from_slice(&fs::read(proposal_path)?)?;
    let confidence = proposal["confidence_milli"].as_u64().unwrap_or_default();
    let safe_evidence = session
        .evidence
        .iter()
        .filter(|item| !secret_like(item))
        .cloned()
        .collect::<Vec<_>>();
    let safe = !safe_evidence.is_empty() && safe_evidence.len() == session.evidence.len();
    let status = if safe && confidence >= MIN_PROBATION_CONFIDENCE_MILLI {
        "probation"
    } else {
        "rejected"
    };

    proposal["lifecycle"] = json!({
        "status": status,
        "auto_promotion": "disabled",
        "promotion": {
            "mode": "explicit_graduation",
            "command": "medusa skills graduate NAME --confirm",
            "requires_probation_state": "passed"
        },
        "rollback": {
            "mode": "graduation_receipt_transaction",
            "on_receipt_failure": "restore_previous_lifecycle_state"
        },
        "minimum_confidence_milli": MIN_PROBATION_CONFIDENCE_MILLI,
        "rejection_reason": if status == "rejected" {
            "insufficient confidence, evidence, or safety"
        } else {
            ""
        },
    });
    proposal["provenance"] = json!({
        "session_id": session.id.to_string(),
        "repository": session.repo.to_string_lossy(),
        "evidence": safe_evidence,
        "evidence_count": session.evidence.len(),
        "verification_result": verification_result(session),
        "completed_at": session.updated_at,
    });

    let path = session
        .repo
        .join(".medusa/memory/lessons")
        .join(format!("{}.json", session.id));
    write_json_atomic(&path, &proposal)?;
    Ok(path)
}

fn processed_marker(session: &AgentSession) -> PathBuf {
    session
        .repo
        .join(".medusa/learning/processed-sessions")
        .join(format!("{}.json", session.id))
}

fn verification_result(session: &AgentSession) -> &'static str {
    if session.completed && skill_outcomes::verification_passed(session) {
        "verified"
    } else {
        "unverified"
    }
}

fn secret_like(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "api_key",
        "apikey",
        "authorization:",
        "bearer ",
        "secret=",
        "token=",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn write_json_atomic(path: &Path, value: &Value) -> MedusaResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use medusa_core::SessionId;
    use medusa_protocol::{Actor, EventPayload};
    use time::OffsetDateTime;

    use crate::evidence::append_event;

    use super::*;

    fn session(repo: &Path) -> AgentSession {
        let mut session = AgentSession {
            id: SessionId::new(),
            objective: "Fix and verify the repository".to_owned(),
            repo: repo.to_path_buf(),
            created_at: OffsetDateTime::now_utc(),
            updated_at: OffsetDateTime::now_utc(),
            completed: true,
            turn: 3,
            plan: Vec::new(),
            pending_question: None,
            messages: Vec::new(),
            events: Vec::new(),
            evidence: vec![
                "cargo test --workspace passed".to_owned(),
                "release smoke passed".to_owned(),
            ],
            tool_artifacts: Vec::new(),
            world_model: None,
            approval_grants: Vec::new(),
            approval_receipts: Vec::new(),
            rollback_receipts: Vec::new(),
        };
        append_event(
            &mut session,
            Actor::System("test".to_owned()),
            EventPayload::VerificationCompleted {
                passed: true,
                evidence: vec!["cargo test --workspace passed".to_owned()],
            },
        )
        .expect("verification event");
        session
    }

    #[test]
    fn processing_is_idempotent_and_writes_canonical_provenance() {
        let repo = tempfile::tempdir().expect("repo");
        let session = session(repo.path());
        process(&session).expect("first processing");
        process(&session).expect("retry processing");

        let memory = repo
            .path()
            .join(".medusa/memory/lessons")
            .join(format!("{}.json", session.id));
        let value: Value =
            serde_json::from_slice(&fs::read(memory).expect("memory")).expect("memory json");
        assert_eq!(value["provenance"]["session_id"], session.id.to_string());
        assert_eq!(value["provenance"]["verification_result"], "verified");
        assert_eq!(value["lifecycle"]["status"], "probation");
        assert_eq!(value["lifecycle"]["auto_promotion"], "disabled");
        assert_eq!(
            value["lifecycle"]["promotion"]["mode"],
            "explicit_graduation"
        );
        assert_eq!(
            value["lifecycle"]["rollback"]["mode"],
            "graduation_receipt_transaction"
        );
        assert!(processed_marker(&session).is_file());
    }

    #[test]
    fn secret_like_evidence_is_rejected_and_not_persisted() {
        let repo = tempfile::tempdir().expect("repo");
        let mut session = session(repo.path());
        session.evidence.push("token=do-not-store".to_owned());
        let proposal = lessons::extract_completed_session(&session)
            .expect("extract")
            .expect("proposal");
        let memory = admit_to_canonical_memory(&session, &proposal).expect("memory");
        let content = fs::read_to_string(&memory).expect("memory file");
        let value: Value = serde_json::from_str(&content).expect("memory json");
        assert_eq!(value["lifecycle"]["status"], "rejected");
        assert!(!content.contains("do-not-store"));
    }
}
