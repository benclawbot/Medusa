use medusa_agent::{AgentSession, record_session_event};
use medusa_core::SessionId;
use medusa_protocol::{
    Actor, EventPayload, SessionAction, SessionActionDeliveryPolicy, SessionActionKind,
    SessionActionLifecycle, SessionActionWakePolicy,
};
use medusa_runtime::frontend::session_action_snapshot;
use serde_json::json;
use time::OffsetDateTime;

fn session(repo: &std::path::Path) -> AgentSession {
    AgentSession {
        id: SessionId::new(),
        objective: "restart action recovery".to_owned(),
        repo: repo.to_path_buf(),
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
        completed: false,
        turn: 0,
        plan: Vec::new(),
        pending_question: None,
        messages: Vec::new(),
        events: Vec::new(),
        evidence: Vec::new(),
        tool_artifacts: Vec::new(),
        world_model: None,
        approval_grants: Vec::new(),
        approval_receipts: Vec::new(),
        rollback_receipts: Vec::new(),
        codex_thread_id: None,
    }
}

fn action(
    session_id: &str,
    id: &str,
    revision: u64,
    kind: SessionActionKind,
    payload: serde_json::Value,
) -> SessionAction {
    SessionAction {
        action_id: format!("action-{id}"),
        idempotency_key: format!("idem-{id}"),
        source: "restart-test".to_owned(),
        target_session_id: session_id.to_owned(),
        expected_session_revision: revision,
        kind,
        delivery_policy: SessionActionDeliveryPolicy::WhenIdle,
        wake_policy: SessionActionWakePolicy::ExternalResume,
        payload,
    }
}

#[test]
fn restart_reconstructs_exactly_one_pending_replacement() {
    let repository = tempfile::tempdir().expect("repository");
    let mut before_restart = session(repository.path());
    let objective = before_restart.objective.clone();
    record_session_event(
        &mut before_restart,
        Actor::Coordinator,
        EventPayload::SessionCreated { objective },
    )
    .expect("session creation");
    let session_id = before_restart.id.to_string();
    let original = action(
        &session_id,
        "original",
        1,
        SessionActionKind::FollowUp,
        json!({"text":"first follow-up"}),
    );
    record_session_event(
        &mut before_restart,
        Actor::User,
        EventPayload::SessionActionAccepted {
            action: original.clone(),
        },
    )
    .expect("original follow-up");
    let replacement = action(
        &session_id,
        "replacement",
        2,
        SessionActionKind::ReplaceFollowUp,
        json!({
            "text":"replacement follow-up",
            "replaces_action_id": original.action_id,
        }),
    );
    record_session_event(
        &mut before_restart,
        Actor::User,
        EventPayload::SessionActionAccepted {
            action: replacement.clone(),
        },
    )
    .expect("replacement follow-up");
    drop(before_restart);

    let after_restart = session_action_snapshot(repository.path(), &session_id)
        .expect("journal-backed action replay after restart");
    assert_eq!(after_restart.queued_count, 1);
    assert_eq!(after_restart.active_action_id, None);
    assert_eq!(after_restart.actions.len(), 2);
    let superseded = after_restart
        .actions
        .iter()
        .find(|view| view.action.action_id == original.action_id)
        .expect("superseded action");
    assert_eq!(superseded.lifecycle, SessionActionLifecycle::Cancelled);
    assert_eq!(
        superseded
            .terminal_evidence
            .as_ref()
            .and_then(|value| value.get("superseded_by"))
            .and_then(serde_json::Value::as_str),
        Some(replacement.action_id.as_str())
    );
    let recoverable = after_restart
        .actions
        .iter()
        .filter(|view| !view.lifecycle.terminal())
        .collect::<Vec<_>>();
    assert_eq!(recoverable.len(), 1);
    assert_eq!(recoverable[0].action.action_id, replacement.action_id);
    assert_eq!(recoverable[0].lifecycle, SessionActionLifecycle::Queued);

    let replayed_again =
        session_action_snapshot(repository.path(), &session_id).expect("idempotent replay");
    assert_eq!(after_restart, replayed_again);
}
