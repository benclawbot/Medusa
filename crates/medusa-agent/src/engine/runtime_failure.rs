use std::{fs, path::PathBuf, thread, time::Duration};

use medusa_confidence::{
    Confidence, ConfidenceObservation, ConfidenceReason, TodoConfidenceHistory, TodoId,
};
use medusa_continuation::{
    ContinuationAction, ContinuationContext, ContinuationController, ContinuationPolicy,
    PlanSnapshot, TodoSnapshot, TodoState,
};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_failure::{
    FailureDecision, FailureDomain, FailureHistory, FailureRecord, FailureSignal, RetryPolicy,
};
use medusa_provider::{Message, MessageBlock, Role};
use time::OffsetDateTime;

use crate::session::{AgentPlanStepStatus, AgentSession, persist, record_terminal_skill_outcome};

pub(crate) enum RuntimeFailureAction {
    Retry,
    Replan,
    Stop,
}

pub(crate) fn handle(
    session: &mut AgentSession,
    error: &MedusaError,
) -> MedusaResult<RuntimeFailureAction> {
    if session.pending_question.is_some() {
        return Ok(RuntimeFailureAction::Stop);
    }

    let mut history = load_history(session);
    let signal = signal_from_error(error)?;
    let decision = history.classify(&signal, RetryPolicy::default());
    history
        .append(FailureRecord {
            sequence: history.records().len() as u32,
            occurred_at: OffsetDateTime::now_utc(),
            signal,
        })
        .map_err(validation_error)?;
    persist_history(session, &history)?;

    let continuation = continuation_decision(session, &decision)?;
    match continuation.action {
        ContinuationAction::Retry { backoff_ms, .. } => {
            if let Some(delay) = backoff_ms {
                thread::sleep(Duration::from_millis(delay));
            }
            persist(session)?;
            Ok(RuntimeFailureAction::Retry)
        }
        ContinuationAction::Replan { reason } => {
            for step in &mut session.plan {
                if step.status != AgentPlanStepStatus::Completed {
                    step.status = AgentPlanStepStatus::Pending;
                }
            }
            session.messages.push(Message {
                role: Role::User,
                content: vec![MessageBlock::Text {
                    text: format!(
                        "Runtime failure policy requires a revised strategy. {reason}. Last error: {error}"
                    ),
                }],
            });
            persist_replan_count(session, performed_replan_count(session).saturating_add(1))?;
            persist(session)?;
            Ok(RuntimeFailureAction::Replan)
        }
        ContinuationAction::Stop { reason } | ContinuationAction::Block { reason } => {
            record_terminal_skill_outcome(session, error, &decision, &reason)?;
            persist(session)?;
            Ok(RuntimeFailureAction::Stop)
        }
        ContinuationAction::Complete
        | ContinuationAction::Resume { .. }
        | ContinuationAction::Spike(_) => {
            record_terminal_skill_outcome(
                session,
                error,
                &decision,
                "failure continuation produced an unsafe terminal action",
            )?;
            persist(session)?;
            Ok(RuntimeFailureAction::Stop)
        }
    }
}

pub(crate) fn record_terminal(
    session: &AgentSession,
    error: &MedusaError,
    reason: &str,
) -> MedusaResult<()> {
    let decision = FailureDecision {
        disposition: medusa_failure::FailureDisposition::Terminal,
        reason: reason.to_owned(),
        attempt: 1,
        remaining_attempts: 0,
        backoff_ms: None,
    };
    record_terminal_skill_outcome(session, error, &decision, reason)?;
    Ok(())
}

fn signal_from_error(error: &MedusaError) -> MedusaResult<FailureSignal> {
    let domain = match error.category {
        ErrorCategory::Validation => FailureDomain::Validation,
        ErrorCategory::Policy => FailureDomain::Policy,
        ErrorCategory::Environment => FailureDomain::Filesystem,
        ErrorCategory::Execution => match error.code {
            ErrorCode::PolicyDenied => FailureDomain::Policy,
            ErrorCode::ToolExecutionFailed => FailureDomain::Tool,
            ErrorCode::DependencyUnavailable
                if error.message.to_ascii_lowercase().contains("user") =>
            {
                FailureDomain::User
            }
            ErrorCode::DependencyUnavailable => FailureDomain::Provider,
            _ => FailureDomain::Internal,
        },
        ErrorCategory::Transient => FailureDomain::Network,
        ErrorCategory::Persistence => FailureDomain::Filesystem,
        ErrorCategory::Internal => FailureDomain::Internal,
    };
    let signal = FailureSignal::new(domain, error.code.to_string(), error.message.clone())
        .map_err(validation_error)?;
    Ok(if error.retryable {
        signal.transient()
    } else {
        signal
    })
}

fn continuation_decision(
    session: &AgentSession,
    failure: &FailureDecision,
) -> MedusaResult<medusa_continuation::ContinuationDecision> {
    let todo_id = TodoId::parse("runtime-step").map_err(validation_error)?;
    let plan = PlanSnapshot {
        plan_id: session.id.to_string(),
        revision: u64::from(session.turn).saturating_add(1),
        captured_at: OffsetDateTime::now_utc(),
        todos: vec![TodoSnapshot {
            id: todo_id.clone(),
            state: TodoState::InProgress,
            dependencies: Vec::new(),
        }],
    };
    let mut confidence = TodoConfidenceHistory::new(todo_id);
    confidence
        .append(
            ConfidenceObservation::new(
                1,
                OffsetDateTime::now_utc(),
                Confidence::from_basis_points(8_000).map_err(validation_error)?,
                ConfidenceReason::ToolFailure,
                "runtime failure classification",
            )
            .map_err(validation_error)?,
        )
        .map_err(validation_error)?;
    ContinuationController::new(ContinuationPolicy::default())
        .map_err(validation_error)?
        .decide(ContinuationContext {
            plan: &plan,
            confidence: &confidence,
            latest_failure: Some(failure),
            automatic_replans: performed_replan_count(session),
            stalled_resumes: 0,
            checkpoint_available: !session.events.is_empty(),
        })
        .map_err(validation_error)
}

fn history_path(session: &AgentSession) -> PathBuf {
    session
        .repo
        .join(".medusa/learning/failure-history")
        .join(format!("{}.json", session.id))
}

fn replan_count_path(session: &AgentSession) -> PathBuf {
    session
        .repo
        .join(".medusa/learning/replan-counts")
        .join(format!("{}.txt", session.id))
}

fn load_history(session: &AgentSession) -> FailureHistory {
    fs::read(history_path(session))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn performed_replan_count(session: &AgentSession) -> u32 {
    fs::read_to_string(replan_count_path(session))
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
}

fn persist_replan_count(session: &AgentSession, count: u32) -> MedusaResult<()> {
    let path = replan_count_path(session);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("txt.tmp");
    fs::write(&temporary, count.to_string())?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn persist_history(session: &AgentSession, history: &FailureHistory) -> MedusaResult<()> {
    let path = history_path(session);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(history)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn validation_error(message: &'static str) -> MedusaError {
    MedusaError::new(ErrorCode::InvalidInput, ErrorCategory::Validation, message)
}
