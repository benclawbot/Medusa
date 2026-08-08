use crate::session::{AgentSession, journal};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_extensions::desktop_commander_tool_is_mutating;
use medusa_protocol::{Actor, EventEnvelope, EventPayload};
use medusa_provider::{mark_pending_route_mutation, record_pending_route_verification};

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

pub(crate) fn append_event(
    session: &mut AgentSession,
    actor: Actor,
    payload: EventPayload,
) -> MedusaResult<()> {
    let mutation_completed = successful_mutation_event(&payload);
    let verification_completed = match &payload {
        EventPayload::VerificationCompleted { passed, .. } => Some(*passed),
        _ => None,
    };

    journal::append_payload_committed(session, actor, payload)?;

    if mutation_completed {
        mark_pending_route_mutation();
    }
    if let Some(passed) = verification_completed {
        record_pending_route_verification(passed)?;
    }
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
}
