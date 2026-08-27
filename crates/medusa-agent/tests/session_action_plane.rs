use std::sync::{Arc, Barrier};

use medusa_agent::{AgentSession, record_session_event, session_browser::load_session};
use medusa_core::SessionId;
use medusa_protocol::{
    Actor, EventPayload, SessionAction, SessionActionDeliveryPolicy, SessionActionKind,
    SessionActionWakePolicy,
};
use serde_json::json;
use time::OffsetDateTime;

fn session(repo: &std::path::Path) -> AgentSession {
    AgentSession {
        id: SessionId::new(),
        objective: "session action integration".to_owned(),
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
    expected_session_revision: u64,
    kind: SessionActionKind,
    payload: serde_json::Value,
) -> SessionAction {
    SessionAction {
        action_id: format!("action-{id}"),
        idempotency_key: format!("idem-{id}"),
        source: "integration-test".to_owned(),
        target_session_id: session_id.to_owned(),
        expected_session_revision,
        kind,
        delivery_policy: if kind == SessionActionKind::Steer {
            SessionActionDeliveryPolicy::NextSafeTurnBoundary
        } else {
            SessionActionDeliveryPolicy::WhenIdle
        },
        wake_policy: SessionActionWakePolicy::OnBoundary,
        payload,
    }
}

#[test]
fn concurrent_replace_and_enqueue_share_one_cas_winner() {
    let repository = tempfile::tempdir().expect("repository");
    let mut base = session(repository.path());
    let objective = base.objective.clone();
    record_session_event(
        &mut base,
        Actor::Coordinator,
        EventPayload::SessionCreated { objective },
    )
    .expect("session creation");
    let session_id = base.id.to_string();
    let original = action(
        &session_id,
        "original",
        1,
        SessionActionKind::FollowUp,
        json!({"text":"original follow-up"}),
    );
    record_session_event(
        &mut base,
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
    let enqueue = action(
        &session_id,
        "enqueue",
        2,
        SessionActionKind::FollowUp,
        json!({"text":"concurrent follow-up"}),
    );
    let barrier = Arc::new(Barrier::new(3));
    let mut replace_writer = base.clone();
    let mut enqueue_writer = base.clone();

    let replace_barrier = Arc::clone(&barrier);
    let replacement_for_thread = replacement.clone();
    let replace_thread = std::thread::spawn(move || {
        replace_barrier.wait();
        record_session_event(
            &mut replace_writer,
            Actor::User,
            EventPayload::SessionActionAccepted {
                action: replacement_for_thread,
            },
        )
        .expect("replacement admission");
    });
    let enqueue_barrier = Arc::clone(&barrier);
    let enqueue_for_thread = enqueue.clone();
    let enqueue_thread = std::thread::spawn(move || {
        enqueue_barrier.wait();
        record_session_event(
            &mut enqueue_writer,
            Actor::User,
            EventPayload::SessionActionAccepted {
                action: enqueue_for_thread,
            },
        )
        .expect("enqueue admission");
    });
    barrier.wait();
    replace_thread.join().expect("replacement writer");
    enqueue_thread.join().expect("enqueue writer");

    let restored = load_session(repository.path(), &session_id).expect("restored journal");
    let accepted = restored
        .events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::SessionActionAccepted { action }
                if action.action_id == replacement.action_id
                    || action.action_id == enqueue.action_id =>
            {
                Some(action.action_id.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let rejected = restored
        .events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::SessionActionRejected { action, reason, .. }
                if action.action_id == replacement.action_id
                    || action.action_id == enqueue.action_id =>
            {
                Some((action.action_id.as_str(), reason.as_str()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        accepted.len(),
        1,
        "only one same-revision action may win CAS"
    );
    assert_eq!(rejected.len(), 1, "the losing action remains auditable");
    assert_eq!(rejected[0].1, "stale_revision");
}

#[test]
fn steering_waits_until_after_the_inflight_tool_boundary() {
    let repository = tempfile::tempdir().expect("repository");
    let mut current = session(repository.path());
    let objective = current.objective.clone();
    record_session_event(
        &mut current,
        Actor::Coordinator,
        EventPayload::SessionCreated { objective },
    )
    .expect("session creation");
    record_session_event(
        &mut current,
        Actor::Coordinator,
        EventPayload::ToolExecutionStarted {
            tool: "shell_run".to_owned(),
        },
    )
    .expect("tool started");
    let session_id = current.id.to_string();
    let steer = action(
        &session_id,
        "steer",
        2,
        SessionActionKind::Steer,
        json!({"text":"use the new constraint"}),
    );
    record_session_event(
        &mut current,
        Actor::User,
        EventPayload::SessionActionAccepted {
            action: steer.clone(),
        },
    )
    .expect("steer admission");
    record_session_event(
        &mut current,
        Actor::User,
        EventPayload::UserFollowupQueued {
            command_id: steer.action_id.clone(),
            prompt: json!({"text":"use the new constraint", "attachments":[], "revision":2}),
        },
    )
    .expect("safe-boundary queue");

    assert!(current.events.iter().all(|event| !matches!(
        &event.payload,
        EventPayload::SessionActionLifecycleChanged { action_id, .. }
            | EventPayload::SessionActionTranscriptLinked { action_id, .. }
            if action_id == &steer.action_id
    )));

    record_session_event(
        &mut current,
        Actor::Coordinator,
        EventPayload::ToolExecutionCompleted {
            tool: "shell_run".to_owned(),
            exit_code: Some(0),
        },
    )
    .expect("tool completed");
    let tool_completed_sequence = current.events.last().expect("tool event").sequence;
    assert!(current.events.iter().all(|event| !matches!(
        &event.payload,
        EventPayload::SessionActionLifecycleChanged { action_id, .. }
            | EventPayload::SessionActionTranscriptLinked { action_id, .. }
            if action_id == &steer.action_id
    )));

    record_session_event(
        &mut current,
        Actor::User,
        EventPayload::UserFollowupDequeued {
            command_id: steer.action_id.clone(),
            text: "use the new constraint".to_owned(),
        },
    )
    .expect("safe-boundary delivery");

    let first_lifecycle_sequence = current
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::SessionActionLifecycleChanged { action_id, .. }
                if action_id == &steer.action_id =>
            {
                Some(event.sequence)
            }
            _ => None,
        })
        .expect("action lifecycle");
    let transcript_sequence = current
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::SessionActionTranscriptLinked {
                action_id,
                transcript_event_sequence,
            } if action_id == &steer.action_id => Some(*transcript_event_sequence),
            _ => None,
        })
        .expect("transcript link");
    assert!(first_lifecycle_sequence > tool_completed_sequence);
    assert!(transcript_sequence > tool_completed_sequence);
}
