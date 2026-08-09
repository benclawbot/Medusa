use crate::session::{AgentSession, journal};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_extensions::desktop_commander_tool_is_mutating;
use medusa_protocol::{
    Actor, EventEnvelope, EventPayload, SessionAction, SessionActionKind, SessionActionLifecycle,
};
use medusa_provider::{
    MessageBlock, clear_pending_route_verification, mark_pending_route_mutation,
    record_pending_route_verification,
};

pub(crate) mod execution_lane {
    include!("execution_lane.rs");
}

fn successful_mutation_event(payload: &EventPayload) -> bool {
    matches!(
        payload,
        EventPayload::ToolExecutionCompleted {
            tool,
            exit_code: Some(0)
        } if matches!(tool.as_str(), "fs_create_dir" | "fs_write" | "patch_apply" | "symbol_rename" | "git_checkpoint")
            || tool
                .strip_prefix("desktop_commander:")
                .is_some_and(desktop_commander_tool_is_mutating)
    )
}

fn abandons_verification_attribution(payload: &EventPayload) -> bool {
    matches!(
        payload,
        EventPayload::SessionReset { .. }
            | EventPayload::CancellationCompleted
            | EventPayload::RuntimeFailed { .. }
            | EventPayload::SessionFailed { .. }
            | EventPayload::SessionCompleted { .. }
    )
}

pub(crate) fn append_event(
    session: &mut AgentSession,
    actor: Actor,
    payload: EventPayload,
) -> MedusaResult<()> {
    if let EventPayload::UserFollowupDequeued { command_id, .. } = &payload {
        refresh_committed_events(session)?;
        if let Some(action) = accepted_action(session, command_id).cloned() {
            deliver_queued_action(session, actor, payload, &action)?;
            return Ok(());
        }
    }

    let mutation_completed = successful_mutation_event(&payload);
    let verification_completed = match &payload {
        EventPayload::VerificationCompleted { passed, .. } => Some(*passed),
        _ => None,
    };
    let attribution_abandoned = abandons_verification_attribution(&payload);
    let cancellation_completed = matches!(payload, EventPayload::CancellationCompleted);
    let runtime_failed = matches!(payload, EventPayload::RuntimeFailed { .. });
    let session_id = session.id.as_str().to_owned();

    journal::append_payload_committed(session, actor, payload)?;

    if cancellation_completed {
        complete_running_cancel_actions(session)?;
    } else if runtime_failed {
        fail_inflight_actions(session)?;
    }

    if mutation_completed {
        mark_pending_route_mutation(&session_id);
    }
    if let Some(passed) = verification_completed {
        record_pending_route_verification(&session_id, passed)?;
    } else if attribution_abandoned {
        clear_pending_route_verification(&session_id);
    }
    Ok(())
}

fn refresh_committed_events(session: &mut AgentSession) -> MedusaResult<()> {
    let committed = crate::session::load(&session.repo, session.id.as_str())?;
    if session.events.len() > committed.events.len()
        || session.events.as_slice() != &committed.events[..session.events.len()]
    {
        return Err(MedusaError::new(
            ErrorCode::InternalInvariant,
            ErrorCategory::Persistence,
            "worker session events diverge from the committed action journal",
        ));
    }
    if committed.events.len() > session.events.len() {
        session
            .events
            .extend_from_slice(&committed.events[session.events.len()..]);
    }
    Ok(())
}

fn accepted_action<'a>(session: &'a AgentSession, action_id: &str) -> Option<&'a SessionAction> {
    session
        .events
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            EventPayload::SessionActionAccepted { action } if action.action_id == action_id => {
                Some(action)
            }
            _ => None,
        })
}

fn action_lifecycle(session: &AgentSession, action_id: &str) -> Option<SessionActionLifecycle> {
    let mut lifecycle = None;
    for event in &session.events {
        match &event.payload {
            EventPayload::SessionActionAccepted { action } if action.action_id == action_id => {
                lifecycle = Some(SessionActionLifecycle::Queued);
            }
            EventPayload::SessionActionLifecycleChanged {
                action_id: changed,
                from,
                to,
                ..
            } if changed == action_id && lifecycle == Some(*from) => lifecycle = Some(*to),
            _ => {}
        }
    }
    lifecycle
}

fn complete_running_cancel_actions(session: &mut AgentSession) -> MedusaResult<()> {
    let cancellation_event_sequence = session.events.last().map_or(0, |event| event.sequence);
    let action_ids = session
        .events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::SessionActionAccepted { action }
                if action.kind == SessionActionKind::Cancel
                    && action_lifecycle(session, &action.action_id)
                        == Some(SessionActionLifecycle::Running) =>
            {
                Some(action.action_id.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    for action_id in action_ids {
        transition(
            session,
            &action_id,
            SessionActionLifecycle::Running,
            SessionActionLifecycle::Completed,
            Some(serde_json::json!({
                "delivery": "cancellation_completed",
                "cancellation_event_sequence": cancellation_event_sequence,
            })),
        )?;
    }
    Ok(())
}

fn fail_inflight_actions(session: &mut AgentSession) -> MedusaResult<()> {
    let failure_event_sequence = session.events.last().map_or(0, |event| event.sequence);
    let action_ids = session
        .events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::SessionActionAccepted { action } => Some(action.action_id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    for action_id in action_ids {
        let Some(lifecycle) = action_lifecycle(session, &action_id) else {
            continue;
        };
        let terminal = match lifecycle {
            SessionActionLifecycle::Selected => Some(SessionActionLifecycle::Cancelled),
            SessionActionLifecycle::Preparing
            | SessionActionLifecycle::Committing
            | SessionActionLifecycle::Running => Some(SessionActionLifecycle::Failed),
            SessionActionLifecycle::Queued
            | SessionActionLifecycle::Completed
            | SessionActionLifecycle::Failed
            | SessionActionLifecycle::Cancelled => None,
        };
        if let Some(terminal) = terminal {
            transition(
                session,
                &action_id,
                lifecycle,
                terminal,
                Some(serde_json::json!({
                    "reason": "runtime_failed",
                    "runtime_failure_event_sequence": failure_event_sequence,
                })),
            )?;
        }
    }
    Ok(())
}

fn deliver_queued_action(
    session: &mut AgentSession,
    actor: Actor,
    transcript_payload: EventPayload,
    action: &SessionAction,
) -> MedusaResult<()> {
    match action_lifecycle(session, &action.action_id) {
        Some(SessionActionLifecycle::Queued) => {
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
        }
        Some(SessionActionLifecycle::Selected) => {
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
        }
        Some(SessionActionLifecycle::Preparing) => {
            transition(
                session,
                &action.action_id,
                SessionActionLifecycle::Preparing,
                SessionActionLifecycle::Committing,
                None,
            )?;
        }
        Some(SessionActionLifecycle::Committing) => {}
        Some(
            SessionActionLifecycle::Running
            | SessionActionLifecycle::Completed
            | SessionActionLifecycle::Failed
            | SessionActionLifecycle::Cancelled,
        ) => {
            return Err(MedusaError::new(
                ErrorCode::InvalidEvent,
                ErrorCategory::Persistence,
                "session action delivery was replayed after leaving committing state",
            ));
        }
        None => {
            return Err(MedusaError::new(
                ErrorCode::InvalidEvent,
                ErrorCategory::Persistence,
                "queued session action has no durable admission",
            ));
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_attribution_uses_same_successful_tool_contract_as_engine() {
        for tool in [
            "fs_create_dir",
            "fs_write",
            "patch_apply",
            "symbol_rename",
            "git_checkpoint",
        ] {
            assert!(successful_mutation_event(
                &EventPayload::ToolExecutionCompleted {
                    tool: tool.to_owned(),
                    exit_code: Some(0),
                }
            ));
        }
        assert!(!successful_mutation_event(
            &EventPayload::ToolExecutionCompleted {
                tool: "fs_read".to_owned(),
                exit_code: Some(0),
            }
        ));
        assert!(!successful_mutation_event(
            &EventPayload::ToolExecutionCompleted {
                tool: "fs_write".to_owned(),
                exit_code: Some(1),
            }
        ));
    }

    #[test]
    fn terminal_session_events_clear_unverified_attribution() {
        assert!(abandons_verification_attribution(
            &EventPayload::SessionReset {
                reason: "new task".to_owned(),
            }
        ));
        assert!(abandons_verification_attribution(
            &EventPayload::CancellationCompleted
        ));
        assert!(!abandons_verification_attribution(
            &EventPayload::SessionPaused {
                reason: "approval".to_owned(),
            }
        ));
    }
}
