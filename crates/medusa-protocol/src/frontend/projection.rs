//! Deterministic projection from the canonical session journal into frontend presentation events.
//!
//! The canonical `EventEnvelope` remains authoritative. This module only derives redacted,
//! user-visible presentation data for every frontend.

use crate::{
    EventEnvelope, EventPayload,
    frontend::{
        FRONTEND_PROTOCOL_VERSION, FrontendEvent, FrontendEventEnvelope, FrontendKind,
        PresentationActivity, PresentationActivityKind, PresentationApproval,
        PresentationLifecycle, PresentationPlanStep, PresentationQuestion,
        PresentationQuestionOption, PresentationWorker,
    },
};
use serde_json::Value;

/// Projects one canonical event into a frontend presentation event.
///
/// `presentation_cursor` is owned by the frontend delivery stream and is intentionally independent
/// from the canonical journal sequence. The caller records the canonical sequence only after the
/// derived event has been delivered successfully.
pub fn project_event(
    event: &EventEnvelope,
    presentation_cursor: u64,
    frontend_kind: FrontendKind,
) -> Option<FrontendEventEnvelope> {
    if presentation_cursor == 0 || event.validate().is_err() {
        return None;
    }
    let frontend = match &event.payload {
        EventPayload::SessionCreated { .. } => FrontendEvent::SubmissionAccepted,
        EventPayload::SessionStateChanged { from, to } => FrontendEvent::Activity(activity(
            event,
            PresentationActivityKind::Progress,
            lifecycle_for_state(*to),
            format!("Session state: {from:?} → {to:?}"),
            Vec::new(),
            None,
        )),
        EventPayload::UserPromptReceived { .. } | EventPayload::UserFollowupDequeued { .. } => {
            FrontendEvent::Started
        }
        EventPayload::UserFollowupQueued { .. } => FrontendEvent::SubmissionQueued { position: 1 },
        EventPayload::SessionActionAccepted { action } => FrontendEvent::Notice {
            severity: "info".to_owned(),
            title: "Session action accepted".to_owned(),
            details: vec![
                format!("Action: {}", action.action_id),
                format!("Kind: {:?}", action.kind),
                format!("Delivery: {:?}", action.delivery_policy),
            ],
        },
        EventPayload::SessionActionRejected {
            action,
            authoritative_revision,
            reason,
        } => FrontendEvent::Notice {
            severity: "warning".to_owned(),
            title: "Session action rejected".to_owned(),
            details: vec![
                format!("Action: {}", action.action_id),
                format!("Reason: {reason}"),
                format!("Authoritative revision: {authoritative_revision}"),
            ],
        },
        EventPayload::SessionActionLifecycleChanged {
            action_id,
            from,
            to,
            ..
        } => FrontendEvent::Activity(activity(
            event,
            PresentationActivityKind::Progress,
            lifecycle_for_action(*to),
            format!("Session action {action_id}: {from:?} → {to:?}"),
            Vec::new(),
            None,
        )),
        EventPayload::SessionActionTranscriptLinked {
            action_id,
            transcript_event_sequence,
        } => FrontendEvent::Notice {
            severity: "info".to_owned(),
            title: "Session action delivered".to_owned(),
            details: vec![
                format!("Action: {action_id}"),
                format!("Authoritative event: {transcript_event_sequence}"),
            ],
        },
        EventPayload::GoalUpdated { objective } => FrontendEvent::Notice {
            severity: "info".to_owned(),
            title: "Goal updated".to_owned(),
            details: vec![objective.clone()],
        },
        EventPayload::ConversationCompacted {
            original_messages,
            retained_messages,
            ..
        } => FrontendEvent::Notice {
            severity: "info".to_owned(),
            title: "Conversation compacted".to_owned(),
            details: vec![format!(
                "Retained {retained_messages} of {original_messages} messages"
            )],
        },
        EventPayload::AssumptionRecorded {
            assumption,
            rationale,
        } => FrontendEvent::Notice {
            severity: "info".to_owned(),
            title: assumption.clone(),
            details: vec![rationale.clone()],
        },
        EventPayload::PlanCreated { plan } | EventPayload::PlanUpdated { update: plan } => {
            FrontendEvent::Plan {
                steps: plan_steps(plan),
                current: current_plan_step(plan),
            }
        }
        EventPayload::QuestionRequested { question } => {
            FrontendEvent::Question(project_question(event, question))
        }
        EventPayload::ApprovalRequested { request } => {
            FrontendEvent::ApprovalRequired(project_approval(event, request))
        }
        EventPayload::ApprovalDecisionRecorded { decision } => FrontendEvent::Activity(activity(
            event,
            PresentationActivityKind::Approval,
            PresentationLifecycle::Succeeded,
            "Approval resolved".to_owned(),
            safe_value_details(decision),
            None,
        )),
        EventPayload::AssistantMessageRecorded { message } => {
            let text = visible_assistant_text(message)?;
            FrontendEvent::AssistantTextDelta { text }
        }
        EventPayload::TeamStateChanged { snapshot } => project_team(snapshot),
        EventPayload::WorkerEvidenceRecorded { evidence } => FrontendEvent::Activity(activity(
            event,
            PresentationActivityKind::Worker,
            PresentationLifecycle::Succeeded,
            "Worker evidence recorded".to_owned(),
            safe_value_details(evidence),
            evidence_reference(evidence),
        )),
        EventPayload::IntegrationReceiptRecorded { receipt } => FrontendEvent::Activity(activity(
            event,
            PresentationActivityKind::Integration,
            PresentationLifecycle::Succeeded,
            "Worker changes integrated".to_owned(),
            safe_value_details(receipt),
            evidence_reference(receipt),
        )),
        EventPayload::RecoveryActionCompleted { receipt } => FrontendEvent::Activity(activity(
            event,
            PresentationActivityKind::Recovery,
            PresentationLifecycle::Succeeded,
            "Recovery action completed".to_owned(),
            safe_value_details(receipt),
            evidence_reference(receipt),
        )),
        EventPayload::CheckpointRestoreRequested {
            checkpoint_id,
            source_cursor,
        } => FrontendEvent::Activity(activity(
            event,
            PresentationActivityKind::Recovery,
            PresentationLifecycle::Active,
            format!("Restoring checkpoint {checkpoint_id}"),
            vec![format!("Source cursor: {source_cursor}")],
            None,
        )),
        EventPayload::CancellationRequested { source } => FrontendEvent::Activity(activity(
            event,
            PresentationActivityKind::Progress,
            PresentationLifecycle::Waiting,
            "Cancellation requested".to_owned(),
            vec![format!("Source: {source}")],
            None,
        )),
        EventPayload::CancellationCompleted => FrontendEvent::Cancelled { reason: None },
        EventPayload::RuntimeTurnFinished => FrontendEvent::TurnFinished,
        EventPayload::RuntimeFailed { message } => FrontendEvent::Failed {
            message: message.clone(),
            recovery: Vec::new(),
        },
        EventPayload::SessionReset { reason } => FrontendEvent::Notice {
            severity: "warning".to_owned(),
            title: "Session reset".to_owned(),
            details: vec![reason.clone()],
        },
        EventPayload::ModelRequestStarted {
            provider, model, ..
        } => FrontendEvent::Activity(activity(
            event,
            PresentationActivityKind::Assistant,
            PresentationLifecycle::Active,
            format!("Requesting {provider}/{model}"),
            Vec::new(),
            None,
        )),
        EventPayload::ModelRequestFailed { .. } => FrontendEvent::Activity(activity(
            event,
            PresentationActivityKind::Assistant,
            PresentationLifecycle::Failed,
            "Model request failed".to_owned(),
            Vec::new(),
            None,
        )),
        EventPayload::ModelResponseReceived { usage, .. } => {
            let input_tokens = integer(usage, &["input_tokens", "inputTokens"]);
            let output_tokens = integer(usage, &["output_tokens", "outputTokens"]);
            FrontendEvent::Usage {
                input_tokens,
                output_tokens,
                total_tokens: input_tokens.saturating_add(output_tokens),
                estimated_cost_microusd: integer(
                    usage,
                    &["estimated_cost_microusd", "estimatedCostMicrousd"],
                ),
            }
        }
        EventPayload::ProviderExecutionRecorded { status } => FrontendEvent::Activity(activity(
            event,
            PresentationActivityKind::Assistant,
            lifecycle_from_value(status),
            "Provider execution".to_owned(),
            safe_value_details(status),
            evidence_reference(status),
        )),
        EventPayload::ToolCallRequested { tool, arguments } => FrontendEvent::Activity(activity(
            event,
            kind_for_tool(tool),
            PresentationLifecycle::Waiting,
            format!("Preparing {tool}"),
            safe_argument_summary(arguments),
            None,
        )),
        EventPayload::ToolCallDenied { tool, reason } => FrontendEvent::Activity(activity(
            event,
            kind_for_tool(tool),
            PresentationLifecycle::Failed,
            format!("Denied {tool}"),
            vec![reason.clone()],
            None,
        )),
        EventPayload::ToolExecutionStarted { tool } => FrontendEvent::Activity(activity(
            event,
            kind_for_tool(tool),
            PresentationLifecycle::Active,
            format!("Running {tool}"),
            Vec::new(),
            None,
        )),
        EventPayload::ToolOutputChunk {
            artifact_ref,
            byte_count,
        } => FrontendEvent::Activity(activity(
            event,
            PresentationActivityKind::Progress,
            PresentationLifecycle::Informational,
            "Tool output available".to_owned(),
            vec![format!("{byte_count} bytes")],
            Some(artifact_ref.clone()),
        )),
        EventPayload::ToolExecutionTimingRecorded { .. } => return None,
        EventPayload::ToolExecutionCompleted { tool, exit_code } => {
            let succeeded = exit_code.is_none_or(|code| code == 0);
            FrontendEvent::Activity(activity(
                event,
                kind_for_tool(tool),
                if succeeded {
                    PresentationLifecycle::Succeeded
                } else {
                    PresentationLifecycle::Failed
                },
                format!("Finished {tool}"),
                exit_code
                    .map(|code| vec![format!("Exit code: {code}")])
                    .unwrap_or_default(),
                None,
            ))
        }
        EventPayload::FileTransactionCommitted {
            paths,
            rollback_ref,
        } => FrontendEvent::Activity(PresentationActivity {
            activity_id: event.event_id.to_string(),
            kind: PresentationActivityKind::Edit,
            lifecycle: PresentationLifecycle::Succeeded,
            title: format!("Committed {} changed path(s)", paths.len()),
            details: Vec::new(),
            affected_paths: paths.clone(),
            evidence_ref: Some(rollback_ref.clone()),
        }),
        EventPayload::CheckpointCreated { checkpoint_id } => FrontendEvent::Activity(activity(
            event,
            PresentationActivityKind::Recovery,
            PresentationLifecycle::Succeeded,
            "Checkpoint created".to_owned(),
            vec![checkpoint_id.clone()],
            None,
        )),
        EventPayload::VerificationStarted { commands } => FrontendEvent::Activity(activity(
            event,
            PresentationActivityKind::Verification,
            PresentationLifecycle::Active,
            "Verification started".to_owned(),
            commands.clone(),
            None,
        )),
        EventPayload::VerificationCompleted { passed, evidence } => {
            FrontendEvent::Activity(activity(
                event,
                PresentationActivityKind::Verification,
                if *passed {
                    PresentationLifecycle::Succeeded
                } else {
                    PresentationLifecycle::Failed
                },
                if *passed {
                    "Verification passed".to_owned()
                } else {
                    "Verification failed".to_owned()
                },
                evidence.clone(),
                evidence.first().cloned(),
            ))
        }
        EventPayload::SessionPaused { reason } => FrontendEvent::Activity(activity(
            event,
            PresentationActivityKind::Progress,
            PresentationLifecycle::Waiting,
            "Waiting for user".to_owned(),
            vec![reason.clone()],
            None,
        )),
        EventPayload::SessionResumed => FrontendEvent::Started,
        EventPayload::SessionCompleted { report_ref } => FrontendEvent::Completed {
            summary: (!report_ref.trim().is_empty()).then(|| format!("Report: {report_ref}")),
        },
        EventPayload::SessionFailed { error } => FrontendEvent::Failed {
            message: error.message.clone(),
            recovery: Vec::new(),
        },
    };

    Some(FrontendEventEnvelope {
        protocol_version: FRONTEND_PROTOCOL_VERSION,
        event_id: format!("{}:{}", event.event_id, frontend_label(frontend_kind)),
        cursor: presentation_cursor,
        session_id: event.session_id.to_string(),
        turn_id: None,
        parent_event_id: None,
        correlation_id: event.correlation_id.to_string(),
        timestamp: event.timestamp,
        lifecycle: lifecycle_for_frontend(&frontend),
        event: frontend,
    })
}

fn activity(
    event: &EventEnvelope,
    kind: PresentationActivityKind,
    lifecycle: PresentationLifecycle,
    title: String,
    details: Vec<String>,
    evidence_ref: Option<String>,
) -> PresentationActivity {
    PresentationActivity {
        activity_id: event.event_id.to_string(),
        kind,
        lifecycle,
        title,
        details: details
            .into_iter()
            .filter(|value| !value.trim().is_empty())
            .take(8)
            .collect(),
        affected_paths: Vec::new(),
        evidence_ref,
    }
}

fn visible_assistant_text(message: &Value) -> Option<String> {
    let role = message.get("role").and_then(Value::as_str)?;
    if role != "assistant" {
        return None;
    }
    let content = message.get("content")?.as_array()?;
    let text = content
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn plan_steps(value: &Value) -> Vec<PresentationPlanStep> {
    plan_array(value)
        .into_iter()
        .flatten()
        .take(32)
        .enumerate()
        .filter_map(|(index, step)| {
            let title = step
                .get("title")
                .and_then(Value::as_str)
                .or_else(|| step.get("task").and_then(Value::as_str))?
                .trim();
            if title.is_empty() {
                return None;
            }
            let status = string(step, &["status", "lifecycle"]).unwrap_or("pending");
            Some(PresentationPlanStep {
                step_id: string(step, &["step_id", "stepId", "id"])
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| format!("step-{}", index.saturating_add(1))),
                title: title.chars().take(240).collect(),
                lifecycle: lifecycle_from_text(status),
            })
        })
        .collect()
}

fn current_plan_step(value: &Value) -> Option<String> {
    plan_array(value)
        .into_iter()
        .flatten()
        .find(|step| {
            string(step, &["status", "lifecycle"]).is_some_and(|value| {
                matches!(value, "in_progress" | "in progress" | "active" | "running")
            })
        })
        .and_then(|step| string(step, &["title", "task"]))
        .map(ToOwned::to_owned)
}

fn plan_array(value: &Value) -> Option<&Vec<Value>> {
    value
        .as_array()
        .or_else(|| value.get("steps").and_then(Value::as_array))
        .or_else(|| value.get("plan").and_then(Value::as_array))
}

fn project_question(event: &EventEnvelope, value: &Value) -> PresentationQuestion {
    let questions = value
        .get("questions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let prompt = if questions.is_empty() {
        string(value, &["question", "prompt"])
            .unwrap_or("Medusa needs your input")
            .to_owned()
    } else {
        questions
            .iter()
            .filter_map(|question| {
                let header = string(question, &["header"]).unwrap_or("Question");
                let body = string(question, &["question", "prompt"])?;
                Some(format!("{header}: {body}"))
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    let options = questions
        .first()
        .and_then(|question| question.get("options"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(12)
        .enumerate()
        .filter_map(|(index, option)| {
            let label = string(option, &["label", "value"])?.trim();
            (!label.is_empty()).then(|| PresentationQuestionOption {
                option_id: format!("option-{}", index.saturating_add(1)),
                label: label.to_owned(),
                value: label.to_owned(),
            })
        })
        .collect();
    PresentationQuestion {
        question_id: format!("question-{}", event.event_id),
        prompt,
        options,
        free_text_allowed: true,
    }
}

fn project_approval(event: &EventEnvelope, value: &Value) -> PresentationApproval {
    let action = string(value, &["tool", "action", "command"])
        .unwrap_or("Protected action")
        .to_owned();
    let scope = value
        .get("grant")
        .and_then(|grant| string(grant, &["scope", "kind"]))
        .or_else(|| string(value, &["scope"]))
        .unwrap_or("single action")
        .to_owned();
    let reason = string(value, &["reason", "description"])
        .unwrap_or("Runtime policy requires explicit approval")
        .to_owned();
    PresentationApproval {
        approval_id: string(
            value,
            &["approval_id", "approvalId", "tool_use_id", "toolUseId"],
        )
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("approval-{}", event.event_id)),
        action,
        scope,
        reason,
        risk: "Protected runtime action".to_owned(),
        expires_at: event.timestamp + time::Duration::minutes(10),
    }
}

fn project_team(value: &Value) -> FrontendEvent {
    let workers = value
        .get("workers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(64)
        .enumerate()
        .map(|(index, worker)| PresentationWorker {
            worker_id: string(worker, &["worker_id", "workerId", "id"])
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| format!("worker-{}", index.saturating_add(1))),
            role: string(worker, &["role"]).unwrap_or("Worker").to_owned(),
            task: string(worker, &["task", "task_id", "taskId"])
                .unwrap_or("Assigned task")
                .to_owned(),
            lifecycle: string(worker, &["lifecycle", "status"])
                .map(lifecycle_from_text)
                .unwrap_or(PresentationLifecycle::Informational),
            current_action: string(
                worker,
                &[
                    "last_update",
                    "lastUpdate",
                    "current_action",
                    "currentAction",
                ],
            )
            .map(ToOwned::to_owned),
        })
        .collect();
    FrontendEvent::Team {
        workers,
        verification: string(value, &["verification"]).map(ToOwned::to_owned),
    }
}

fn kind_for_tool(tool: &str) -> PresentationActivityKind {
    let normalized = tool.to_ascii_lowercase();
    if normalized.contains("test") || normalized.contains("verify") {
        PresentationActivityKind::Test
    } else if normalized.contains("read")
        || normalized.contains("search")
        || normalized.contains("list")
    {
        PresentationActivityKind::RepositoryRead
    } else if normalized.contains("write")
        || normalized.contains("patch")
        || normalized.contains("edit")
        || normalized.contains("create")
        || normalized.contains("rename")
    {
        PresentationActivityKind::Edit
    } else {
        PresentationActivityKind::Command
    }
}

fn safe_argument_summary(value: &Value) -> Vec<String> {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    object
        .iter()
        .filter(|(key, _)| !secret_like(key))
        .filter_map(|(key, value)| {
            let rendered = match value {
                Value::String(text) if text.len() <= 240 => text.clone(),
                Value::Number(number) => number.to_string(),
                Value::Bool(flag) => flag.to_string(),
                _ => return None,
            };
            Some(format!("{key}: {rendered}"))
        })
        .take(6)
        .collect()
}

fn safe_value_details(value: &Value) -> Vec<String> {
    string(value, &["summary", "message", "status", "result"])
        .and_then(safe_presentation_summary)
        .into_iter()
        .collect()
}

fn safe_presentation_summary(summary: &str) -> Option<String> {
    const OPEN: &str = "<think>";
    const CLOSE: &str = "</think>";

    let mut remaining = summary;
    let mut visible = String::with_capacity(summary.len());
    loop {
        let Some(start) = remaining.find(OPEN) else {
            visible.push_str(remaining);
            break;
        };
        visible.push_str(&remaining[..start]);
        let after_open = &remaining[start + OPEN.len()..];
        let Some(end) = after_open.find(CLOSE) else {
            break;
        };
        remaining = &after_open[end + CLOSE.len()..];
    }

    let visible = visible.replace(CLOSE, "");
    let visible = visible.trim();
    (!visible.is_empty()).then(|| visible.chars().take(320).collect())
}

fn evidence_reference(value: &Value) -> Option<String> {
    string(
        value,
        &[
            "evidence_ref",
            "evidenceRef",
            "artifact_ref",
            "artifactRef",
            "report_ref",
            "reportRef",
        ],
    )
    .map(ToOwned::to_owned)
}

fn secret_like(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "token",
        "secret",
        "password",
        "credential",
        "authorization",
        "api_key",
        "apikey",
    ]
    .iter()
    .any(|candidate| key.contains(candidate))
}

fn integer(value: &Value, keys: &[&str]) -> u64 {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_u64))
        .unwrap_or_default()
}

fn string<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
}

fn lifecycle_from_value(value: &Value) -> PresentationLifecycle {
    string(value, &["status", "lifecycle"])
        .map(lifecycle_from_text)
        .unwrap_or(PresentationLifecycle::Informational)
}

fn lifecycle_from_text(value: &str) -> PresentationLifecycle {
    match value.trim().to_ascii_lowercase().as_str() {
        "active" | "running" | "in_progress" | "in progress" | "started" => {
            PresentationLifecycle::Active
        }
        "waiting" | "pending" | "queued" | "blocked" | "paused" => PresentationLifecycle::Waiting,
        "succeeded" | "success" | "completed" | "complete" | "integrated" | "passed" => {
            PresentationLifecycle::Succeeded
        }
        "failed" | "error" | "denied" => PresentationLifecycle::Failed,
        "cancelled" | "canceled" | "cancellation_requested" => PresentationLifecycle::Cancelled,
        _ => PresentationLifecycle::Informational,
    }
}

fn lifecycle_for_state(state: crate::SessionState) -> PresentationLifecycle {
    use crate::SessionState;
    match state {
        SessionState::Completed => PresentationLifecycle::Succeeded,
        SessionState::Cancelled => PresentationLifecycle::Cancelled,
        SessionState::Crashed | SessionState::BudgetExhausted => PresentationLifecycle::Failed,
        SessionState::Blocked | SessionState::Paused | SessionState::CancelRequested => {
            PresentationLifecycle::Waiting
        }
        _ => PresentationLifecycle::Active,
    }
}

fn lifecycle_for_action(state: crate::SessionActionLifecycle) -> PresentationLifecycle {
    use crate::SessionActionLifecycle;
    match state {
        SessionActionLifecycle::Queued
        | SessionActionLifecycle::Selected
        | SessionActionLifecycle::Preparing
        | SessionActionLifecycle::Committing => PresentationLifecycle::Waiting,
        SessionActionLifecycle::Running => PresentationLifecycle::Active,
        SessionActionLifecycle::Completed => PresentationLifecycle::Succeeded,
        SessionActionLifecycle::Failed => PresentationLifecycle::Failed,
        SessionActionLifecycle::Cancelled => PresentationLifecycle::Cancelled,
    }
}

fn frontend_label(frontend: FrontendKind) -> &'static str {
    match frontend {
        FrontendKind::Tui => "tui",
        FrontendKind::Desktop => "desktop",
        FrontendKind::Telegram => "telegram",
        FrontendKind::Headless => "headless",
        FrontendKind::Other => "other",
    }
}

fn lifecycle_for_frontend(event: &FrontendEvent) -> PresentationLifecycle {
    match event {
        FrontendEvent::Completed { .. } => PresentationLifecycle::Succeeded,
        FrontendEvent::Cancelled { .. } => PresentationLifecycle::Cancelled,
        FrontendEvent::Failed { .. } => PresentationLifecycle::Failed,
        FrontendEvent::Question(_)
        | FrontendEvent::ApprovalRequired(_)
        | FrontendEvent::SubmissionQueued { .. } => PresentationLifecycle::Waiting,
        FrontendEvent::Activity(activity) => activity.lifecycle,
        FrontendEvent::TurnFinished
        | FrontendEvent::Usage { .. }
        | FrontendEvent::SettingsChanged { .. }
        | FrontendEvent::Notice { .. }
        | FrontendEvent::Artifact(_) => PresentationLifecycle::Informational,
        _ => PresentationLifecycle::Active,
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        Actor, EventEnvelope, EventPayload, SessionAction, SessionActionDeliveryPolicy,
        SessionActionKind, SessionActionLifecycle, SessionActionWakePolicy,
    };
    use medusa_core::{CorrelationId, SessionId};
    use serde_json::json;
    use time::OffsetDateTime;

    use super::*;

    fn event(payload: EventPayload) -> EventEnvelope {
        EventEnvelope::new(
            1,
            SessionId::new(),
            Actor::Coordinator,
            CorrelationId::new(),
            payload,
            None,
            OffsetDateTime::UNIX_EPOCH,
        )
        .expect("event")
    }

    fn action() -> SessionAction {
        SessionAction {
            action_id: "action-1".to_owned(),
            idempotency_key: "idempotency-1".to_owned(),
            source: "test".to_owned(),
            target_session_id: "session-1".to_owned(),
            expected_session_revision: 4,
            kind: SessionActionKind::Steer,
            delivery_policy: SessionActionDeliveryPolicy::NextSafeTurnBoundary,
            wake_policy: SessionActionWakePolicy::OnBoundary,
            payload: json!({"text":"do not expose this payload"}),
        }
    }

    #[test]
    fn assistant_projection_excludes_tool_arguments() {
        let source = event(EventPayload::AssistantMessageRecorded {
            message: json!({
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "Visible answer"},
                    {"type": "tool_use", "id": "1", "name": "shell", "input": {"token": "secret"}}
                ]
            }),
        });
        let projected = project_event(&source, 1, FrontendKind::Telegram).expect("projection");
        assert_eq!(
            projected.event,
            FrontendEvent::AssistantTextDelta {
                text: "Visible answer".to_owned()
            }
        );
    }

    #[test]
    fn session_action_events_are_projected_for_every_frontend() {
        let source = event(EventPayload::SessionActionAccepted { action: action() });
        for frontend in [
            FrontendKind::Tui,
            FrontendKind::Desktop,
            FrontendKind::Telegram,
            FrontendKind::Headless,
        ] {
            let projected = project_event(&source, 4, frontend).expect("action projection");
            let FrontendEvent::Notice { details, .. } = projected.event else {
                panic!("expected action notice")
            };
            assert!(details.iter().any(|detail| detail.contains("action-1")));
            assert!(
                details
                    .iter()
                    .all(|detail| !detail.contains("do not expose"))
            );
        }

        let lifecycle = project_event(
            &event(EventPayload::SessionActionLifecycleChanged {
                action_id: "action-1".to_owned(),
                from: SessionActionLifecycle::Queued,
                to: SessionActionLifecycle::Selected,
                evidence: None,
            }),
            5,
            FrontendKind::Tui,
        )
        .expect("lifecycle projection");
        assert_eq!(lifecycle.lifecycle, PresentationLifecycle::Waiting);
    }

    #[test]
    fn rejected_action_projection_is_identical_for_desktop_and_telegram() {
        let source = event(EventPayload::SessionActionRejected {
            action: action(),
            authoritative_revision: 9,
            reason: "stale_revision".to_owned(),
        });
        let desktop = project_event(&source, 7, FrontendKind::Desktop).expect("desktop projection");
        let telegram =
            project_event(&source, 7, FrontendKind::Telegram).expect("telegram projection");
        assert_eq!(desktop.event, telegram.event);
        assert_eq!(desktop.lifecycle, telegram.lifecycle);
        assert_eq!(desktop.cursor, telegram.cursor);
        assert!(desktop.event_id.ends_with(":desktop"));
        assert!(telegram.event_id.ends_with(":telegram"));
        let FrontendEvent::Notice {
            severity, details, ..
        } = desktop.event
        else {
            panic!("expected rejection notice")
        };
        assert_eq!(severity, "warning");
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("stale_revision"))
        );
        assert!(details.iter().any(|detail| detail.contains("9")));
    }

    #[test]
    fn plan_question_and_team_shapes_are_projected() {
        let plan = project_event(
            &event(EventPayload::PlanCreated {
                plan: json!({"steps": [{"title": "Inspect", "status": "in_progress"}]}),
            }),
            1,
            FrontendKind::Telegram,
        )
        .expect("plan");
        assert!(matches!(plan.event, FrontendEvent::Plan { ref steps, .. } if steps.len() == 1));

        let question = project_event(
            &event(EventPayload::QuestionRequested {
                question: json!({
                    "questions": [{
                        "header": "Choice",
                        "question": "Proceed?",
                        "options": [{"label": "Yes", "description": "Continue"}],
                        "multi_select": false
                    }]
                }),
            }),
            2,
            FrontendKind::Telegram,
        )
        .expect("question");
        assert!(
            matches!(question.event, FrontendEvent::Question(ref value) if value.options.len() == 1)
        );

        let team = project_event(
            &event(EventPayload::TeamStateChanged {
                snapshot: json!({
                    "workers": [{
                        "workerId": "worker-1",
                        "role": "Implementer",
                        "taskId": "telegram",
                        "lifecycle": "running",
                        "lastUpdate": "Editing renderer"
                    }]
                }),
            }),
            3,
            FrontendKind::Telegram,
        )
        .expect("team");
        assert!(
            matches!(team.event, FrontendEvent::Team { ref workers, .. } if workers.len() == 1)
        );
    }

    #[test]
    fn frontend_identity_is_scoped_without_changing_payload() {
        let source = event(EventPayload::RuntimeTurnFinished);
        let tui = project_event(&source, 4, FrontendKind::Tui).expect("tui");
        let desktop = project_event(&source, 4, FrontendKind::Desktop).expect("desktop");
        assert_eq!(tui.event, desktop.event);
        assert_eq!(tui.cursor, desktop.cursor);
        assert!(tui.event_id.ends_with(":tui"));
        assert!(desktop.event_id.ends_with(":desktop"));
    }

    #[test]
    fn tui_worker_evidence_hides_complete_reasoning_and_preserves_visible_result() {
        let projected = project_event(
            &event(EventPayload::WorkerEvidenceRecorded {
                evidence: json!({
                    "summary": "<think>private provider reasoning</think>\nApplied the requested fix",
                    "evidence_ref": "artifact://worker-1"
                }),
            }),
            1,
            FrontendKind::Tui,
        )
        .expect("projection");
        let FrontendEvent::Activity(activity) = projected.event else {
            panic!("expected worker activity")
        };
        assert_eq!(activity.details, vec!["Applied the requested fix"]);
        assert_eq!(
            activity.evidence_ref.as_deref(),
            Some("artifact://worker-1")
        );
        assert!(
            activity
                .details
                .iter()
                .all(|detail| !detail.contains("think"))
        );
        assert!(
            activity
                .details
                .iter()
                .all(|detail| !detail.contains("private provider reasoning"))
        );
    }

    #[test]
    fn worker_evidence_drops_unclosed_reasoning_tail() {
        for summary in [
            "<think>private provider reasoning without a closing tag",
            "Public result<think>private trailing reasoning",
        ] {
            let projected = project_event(
                &event(EventPayload::WorkerEvidenceRecorded {
                    evidence: json!({"summary": summary, "evidence_ref": "artifact://worker-2"}),
                }),
                2,
                FrontendKind::Tui,
            )
            .expect("projection");
            let FrontendEvent::Activity(activity) = projected.event else {
                panic!("expected worker activity")
            };
            assert!(
                activity
                    .details
                    .iter()
                    .all(|detail| !detail.contains("private"))
            );
            assert!(
                activity
                    .details
                    .iter()
                    .all(|detail| !detail.contains("<think>"))
            );
            if summary.starts_with("Public result") {
                assert_eq!(activity.details, vec!["Public result"]);
            } else {
                assert!(activity.details.is_empty());
            }
            assert_eq!(
                activity.evidence_ref.as_deref(),
                Some("artifact://worker-2")
            );
        }
    }

    #[test]
    fn secret_like_tool_arguments_are_not_projected() {
        let projected = project_event(
            &event(EventPayload::ToolCallRequested {
                tool: "shell_run".to_owned(),
                arguments: json!({"command": "cargo test", "api_token": "do-not-render"}),
            }),
            1,
            FrontendKind::Telegram,
        )
        .expect("projection");
        let FrontendEvent::Activity(activity) = projected.event else {
            panic!("expected activity")
        };
        assert_eq!(activity.details, vec!["command: cargo test"]);
    }
}
