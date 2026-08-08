use crate::session::{AgentSession, journal};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_protocol::{
    Actor, EventEnvelope, EventPayload, SessionAction, SessionActionKind, SessionActionLifecycle,
};
use medusa_provider::MessageBlock;

pub(crate) mod execution_lane {
    include!("execution_lane.rs");
}

pub(crate) fn append_event(
    session: &mut AgentSession,
    actor: Actor,
    payload: EventPayload,
) -> MedusaResult<()> {
    if let EventPayload::UserFollowupDequeued { command_id, .. } = &payload
        && let Some(action) = accepted_action(session, command_id).cloned()
    {
        deliver_queued_action(session, actor, payload, &action)?;
        return Ok(());
    }
    journal::append_payload_committed(session, actor, payload)?;
    Ok(())
}

fn accepted_action<'a>(session: &'a AgentSession, action_id: &str) -> Option<&'a SessionAction> {
    session.events.iter().rev().find_map(|event| match &event.payload {
        EventPayload::SessionActionAccepted { action } if action.action_id == action_id => {
            Some(action)
        }
        _ => None,
    })
}

fn deliver_queued_action(
    session: &mut AgentSession,
    actor: Actor,
    transcript_payload: EventPayload,
    action: &SessionAction,
) -> MedusaResult<()> {
    transition(
        session,
        &action.action_id,
        SessionActionLifecycle::Queued,
        SessionActionLifecycle::Selected,
        None,
    )?;
    transition(
        session,
        &action.action_id,
        SessionActionLifecycle::Selected,
        SessionActionLifecycle::Preparing,
        None,
    )?;
    transition(
        session,
        &action.action_id,
        SessionActionLifecycle::Preparing,
        SessionActionLifecycle::Committing,
        None,
    )?;

    if action.kind == SessionActionKind::GoalAdjustment {
        let objective = action
            .payload
            .get("objective")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                MedusaError::new(
                    ErrorCode::InvalidEvent,
                    ErrorCategory::Validation,
                    "goal-adjustment action is missing a non-empty objective",
                )
            })?
            .to_owned();
        session.objective.clone_from(&objective);
        if let Some(message) = session.messages.last_mut()
            && let Some(MessageBlock::Text { text }) = message.content.first_mut()
            && text.starts_with("Current session goal:")
        {
            *text = format!("Current session goal: {objective}");
        }
        journal::append_payload_committed(
            session,
            Actor::User,
            EventPayload::GoalUpdated { objective },
        )?;
    }

    journal::append_payload_committed(session, actor, transcript_payload)?;
    let transcript_event_sequence = session
        .events
        .last()
        .map(|event| event.sequence)
        .ok_or_else(|| {
            MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                "action transcript append did not produce a journal event",
            )
        })?;
    journal::append_payload_committed(
        session,
        Actor::Coordinator,
        EventPayload::SessionActionTranscriptLinked {
            action_id: action.action_id.clone(),
            transcript_event_sequence,
        },
    )?;
    transition(
        session,
        &action.action_id,
        SessionActionLifecycle::Committing,
        SessionActionLifecycle::Running,
        Some(serde_json::json!({
            "transcript_event_sequence": transcript_event_sequence,
        })),
    )?;
    transition(
        session,
        &action.action_id,
        SessionActionLifecycle::Running,
        SessionActionLifecycle::Completed,
        Some(serde_json::json!({
            "delivery": "authoritative_transcript",
            "transcript_event_sequence": transcript_event_sequence,
        })),
    )?;
    Ok(())
}

fn transition(
    session: &mut AgentSession,
    action_id: &str,
    from: SessionActionLifecycle,
    to: SessionActionLifecycle,
    evidence: Option<serde_json::Value>,
) -> MedusaResult<()> {
    if !from.can_transition_to(to) {
        return Err(MedusaError::new(
            ErrorCode::InvalidEvent,
            ErrorCategory::Validation,
            format!("invalid session action lifecycle transition {from:?} -> {to:?}"),
        ));
    }
    journal::append_payload_committed(
        session,
        Actor::Coordinator,
        EventPayload::SessionActionLifecycleChanged {
            action_id: action_id.to_owned(),
            from,
            to,
            evidence,
        },
    )?;
    Ok(())
}

pub(crate) fn verify_chain(events: &[EventEnvelope]) -> MedusaResult<()> {
    // Keep the #685 lane contract linked into production until the entrypoint wiring tranche.
    let _lane_selector = execution_lane::select_execution_lane;
    let mut previous: Option<&str> = None;
    for event in events {
        event.validate()?;
        if event.previous_hash.as_deref() != previous {
            return Err(MedusaError::new(
                ErrorCode::ChecksumMismatch,
                ErrorCategory::Persistence,
                "event chain previous hash mismatch",
            ));
        }
        previous = Some(&event.checksum);
    }
    Ok(())
}
