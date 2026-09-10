//! Deterministic continuation decisions for incomplete autonomous plans.

use medusa_confidence::{
    GateDecision, SpikeGatePolicy, SpikeRequest, TodoConfidenceHistory, TodoId,
};
use medusa_failure::{FailureDecision, FailureDisposition};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoState {
    Pending,
    InProgress,
    Completed,
    Failed,
    Blocked,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TodoSnapshot {
    pub id: TodoId,
    pub state: TodoState,
    #[serde(default)]
    pub dependencies: Vec<TodoId>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlanSnapshot {
    pub plan_id: String,
    pub revision: u64,
    pub captured_at: OffsetDateTime,
    #[serde(default)]
    pub todos: Vec<TodoSnapshot>,
}

impl PlanSnapshot {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.plan_id.trim().is_empty() {
            return Err("plan identifier cannot be empty");
        }
        if self.revision == 0 {
            return Err("plan revision must start at one");
        }
        if self.todos.is_empty() {
            return Err("plan must contain at least one todo");
        }
        for (index, todo) in self.todos.iter().enumerate() {
            if self.todos[..index]
                .iter()
                .any(|candidate| candidate.id == todo.id)
            {
                return Err("plan contains duplicate todo identifiers");
            }
            if todo
                .dependencies
                .iter()
                .any(|dependency| dependency == &todo.id)
            {
                return Err("todo cannot depend on itself");
            }
            if todo.dependencies.iter().any(|dependency| {
                !self
                    .todos
                    .iter()
                    .any(|candidate| &candidate.id == dependency)
            }) {
                return Err("todo references an unknown dependency");
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn complete(&self) -> bool {
        self.todos
            .iter()
            .all(|todo| todo.state == TodoState::Completed)
    }

    #[must_use]
    pub fn next_runnable(&self) -> Option<&TodoSnapshot> {
        self.todos.iter().find(|todo| {
            matches!(todo.state, TodoState::Pending | TodoState::InProgress)
                && todo.dependencies.iter().all(|dependency| {
                    self.todos
                        .iter()
                        .find(|candidate| &candidate.id == dependency)
                        .is_some_and(|candidate| candidate.state == TodoState::Completed)
                })
        })
    }

    #[must_use]
    pub fn has_unresolved_blocker(&self) -> bool {
        self.todos
            .iter()
            .any(|todo| todo.state == TodoState::Blocked)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContinuationPolicy {
    pub max_automatic_replans: u32,
    pub max_stalled_resumes: u32,
    pub spike_gate: SpikeGatePolicy,
}

impl Default for ContinuationPolicy {
    fn default() -> Self {
        Self {
            max_automatic_replans: 2,
            max_stalled_resumes: 2,
            spike_gate: SpikeGatePolicy::default(),
        }
    }
}

impl ContinuationPolicy {
    pub fn validate(self) -> Result<Self, &'static str> {
        if self.max_automatic_replans == 0 {
            return Err("max_automatic_replans must be greater than zero");
        }
        if self.max_stalled_resumes == 0 {
            return Err("max_stalled_resumes must be greater than zero");
        }
        Ok(self)
    }
}

#[derive(Clone, Debug)]
pub struct ContinuationContext<'a> {
    pub plan: &'a PlanSnapshot,
    pub confidence: &'a TodoConfidenceHistory,
    pub latest_failure: Option<&'a FailureDecision>,
    pub automatic_replans: u32,
    pub stalled_resumes: u32,
    pub checkpoint_available: bool,
}

/// Schema version of [`ContinuationAction`] / [`ContinuationDecision`].
///
/// Bump this whenever a variant is added, removed, or reshaped; persisted or
/// transmitted decisions carrying any other version must be rejected, never
/// coerced.
pub const CONTINUATION_ACTION_VERSION: u32 = 1;

/// A single deterministic continuation step selected for an incomplete plan.
///
/// Unknown-field policy (fail-closed): the `action` tag must be a known
/// variant and `details` must contain exactly that variant's fields —
/// enforced by `deny_unknown_fields`. A provider or peer that invents a new
/// action (or a new field) gets a deserialization error instead of a silently
/// reinterpreted decision. Adding a variant or field requires bumping
/// [`CONTINUATION_ACTION_VERSION`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "action", content = "details")]
#[serde(deny_unknown_fields)]
pub enum ContinuationAction {
    Complete,
    Resume {
        todo_id: TodoId,
        from_checkpoint: bool,
    },
    Retry {
        todo_id: TodoId,
        backoff_ms: Option<u64>,
    },
    Replan {
        reason: String,
    },
    Spike(SpikeRequest),
    Block {
        reason: String,
    },
    Stop {
        reason: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContinuationDecision {
    /// Schema version; always [`CONTINUATION_ACTION_VERSION`] for decisions
    /// produced by [`ContinuationController::decide`].
    pub version: u32,
    pub action: ContinuationAction,
    pub plan_revision: u64,
    pub reason: String,
}

impl ContinuationDecision {
    /// Renders the stable single-line audit record for this decision.
    ///
    /// Callers must emit this whenever a plain `InProgress` resume is taken
    /// (the highest-volume, lowest-signal branch): without it, resume loops
    /// are invisible in operator logs. Format is `key=value` pairs so it
    /// greps cleanly: `continuation_decision version=.. plan_revision=..
    /// action=.. reason=..`.
    #[must_use]
    pub fn audit_line(&self) -> String {
        let action = match &self.action {
            ContinuationAction::Complete => "complete".to_owned(),
            ContinuationAction::Resume {
                todo_id,
                from_checkpoint,
            } => format!("resume:{}:checkpoint={}", todo_id.as_str(), from_checkpoint),
            ContinuationAction::Retry {
                todo_id,
                backoff_ms,
            } => format!(
                "retry:{}:backoff_ms={}",
                todo_id.as_str(),
                backoff_ms.map_or("none".to_owned(), |ms| ms.to_string())
            ),
            ContinuationAction::Replan { .. } => "replan".to_owned(),
            ContinuationAction::Spike(_) => "spike".to_owned(),
            ContinuationAction::Block { .. } => "block".to_owned(),
            ContinuationAction::Stop { .. } => "stop".to_owned(),
        };
        format!(
            "continuation_decision version={} plan_revision={} action={} reason={}",
            self.version, self.plan_revision, action, self.reason
        )
    }
}

pub struct ContinuationController {
    policy: ContinuationPolicy,
}

impl ContinuationController {
    pub fn new(policy: ContinuationPolicy) -> Result<Self, &'static str> {
        Ok(Self {
            policy: policy.validate()?,
        })
    }

    pub fn decide(
        &self,
        context: ContinuationContext<'_>,
    ) -> Result<ContinuationDecision, &'static str> {
        context.plan.validate()?;
        context.confidence.validate()?;

        if context.plan.complete() {
            return Ok(self.decision(
                context.plan,
                ContinuationAction::Complete,
                "all plan items are complete",
            ));
        }

        if context.plan.has_unresolved_blocker() {
            return Ok(self.decision(
                context.plan,
                ContinuationAction::Block {
                    reason: "plan contains an unresolved blocked todo".to_owned(),
                },
                "automatic continuation cannot bypass explicit blockers",
            ));
        }

        if let Some(failure) = context.latest_failure {
            match failure.disposition {
                FailureDisposition::Terminal => {
                    return Ok(self.decision(
                        context.plan,
                        ContinuationAction::Stop {
                            reason: failure.reason.clone(),
                        },
                        "latest failure is terminal",
                    ));
                }
                FailureDisposition::Replan => {
                    if context.automatic_replans >= self.policy.max_automatic_replans {
                        return Ok(self.decision(
                            context.plan,
                            ContinuationAction::Stop {
                                reason: "automatic replan budget exhausted".to_owned(),
                            },
                            "continuation stopped to prevent a replan loop",
                        ));
                    }
                    return Ok(self.decision(
                        context.plan,
                        ContinuationAction::Replan {
                            reason: failure.reason.clone(),
                        },
                        "failure classifier invalidated the current strategy",
                    ));
                }
                FailureDisposition::RetryImmediately | FailureDisposition::RetryWithBackoff => {
                    // Retries are resumes too: an exhausted stall budget must
                    // force a replan here exactly as on the checkpoint path
                    // below, or a failing todo spins forever.
                    if context.stalled_resumes >= self.policy.max_stalled_resumes {
                        return Ok(self.decision(
                            context.plan,
                            ContinuationAction::Replan {
                                reason: "resume made no durable progress".to_owned(),
                            },
                            "stalled resume budget exhausted",
                        ));
                    }
                    let todo = context
                        .plan
                        .next_runnable()
                        .ok_or("incomplete plan has no runnable todo")?;
                    return Ok(self.decision(
                        context.plan,
                        ContinuationAction::Retry {
                            todo_id: todo.id.clone(),
                            backoff_ms: failure.backoff_ms,
                        },
                        "failure classifier approved a bounded retry",
                    ));
                }
            }
        }

        let todo = context
            .plan
            .next_runnable()
            .ok_or("incomplete plan has no runnable todo")?;
        if todo.id != context.confidence.todo_id {
            return Err("confidence history does not match the next runnable todo");
        }

        match self.policy.spike_gate.evaluate(context.confidence) {
            GateDecision::Spike(request) => Ok(self.decision(
                context.plan,
                ContinuationAction::Spike(request),
                "todo confidence requires a bounded investigation spike",
            )),
            GateDecision::Execute => {
                if context.stalled_resumes >= self.policy.max_stalled_resumes {
                    return Ok(self.decision(
                        context.plan,
                        ContinuationAction::Replan {
                            reason: "resume made no durable progress".to_owned(),
                        },
                        "stalled resume budget exhausted",
                    ));
                }
                Ok(self.decision(
                    context.plan,
                    ContinuationAction::Resume {
                        todo_id: todo.id.clone(),
                        from_checkpoint: context.checkpoint_available,
                    },
                    if context.checkpoint_available {
                        "resume the next runnable todo from the latest validated checkpoint"
                    } else {
                        "start the next runnable todo without a prior checkpoint"
                    },
                ))
            }
        }
    }

    fn decision(
        &self,
        plan: &PlanSnapshot,
        action: ContinuationAction,
        reason: impl Into<String>,
    ) -> ContinuationDecision {
        ContinuationDecision {
            version: CONTINUATION_ACTION_VERSION,
            action,
            plan_revision: plan.revision,
            reason: reason.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use medusa_confidence::{Confidence, ConfidenceObservation, ConfidenceReason};
    use medusa_failure::FailureDisposition;
    use time::macros::datetime;

    fn id(value: &str) -> TodoId {
        TodoId::parse(value).expect("todo id")
    }

    fn plan(state: TodoState) -> PlanSnapshot {
        PlanSnapshot {
            plan_id: "plan-1".to_owned(),
            revision: 1,
            captured_at: datetime!(2026-07-24 10:00 UTC),
            todos: vec![TodoSnapshot {
                id: id("implement"),
                state,
                dependencies: Vec::new(),
            }],
        }
    }

    fn confidence(value: u16) -> TodoConfidenceHistory {
        let mut history = TodoConfidenceHistory::new(id("implement"));
        history
            .append(
                ConfidenceObservation::new(
                    1,
                    datetime!(2026-07-24 10:00 UTC),
                    Confidence::from_basis_points(value).expect("confidence"),
                    ConfidenceReason::InitialEstimate,
                    "estimate",
                )
                .expect("observation"),
            )
            .expect("append");
        history
    }

    fn context<'a>(
        plan: &'a PlanSnapshot,
        confidence: &'a TodoConfidenceHistory,
    ) -> ContinuationContext<'a> {
        ContinuationContext {
            plan,
            confidence,
            latest_failure: None,
            automatic_replans: 0,
            stalled_resumes: 0,
            checkpoint_available: true,
        }
    }

    #[test]
    fn resumes_incomplete_plan_from_checkpoint() {
        let controller =
            ContinuationController::new(ContinuationPolicy::default()).expect("controller");
        let plan = plan(TodoState::InProgress);
        let confidence = confidence(8_000);
        let decision = controller
            .decide(context(&plan, &confidence))
            .expect("decision");
        assert!(matches!(
            decision.action,
            ContinuationAction::Resume {
                from_checkpoint: true,
                ..
            }
        ));
    }

    #[test]
    fn low_confidence_requires_spike() {
        let controller =
            ContinuationController::new(ContinuationPolicy::default()).expect("controller");
        let plan = plan(TodoState::Pending);
        let confidence = confidence(4_000);
        let decision = controller
            .decide(context(&plan, &confidence))
            .expect("decision");
        assert!(matches!(decision.action, ContinuationAction::Spike(_)));
    }

    #[test]
    fn retryable_failure_preserves_backoff() {
        let controller =
            ContinuationController::new(ContinuationPolicy::default()).expect("controller");
        let plan = plan(TodoState::InProgress);
        let confidence = confidence(8_000);
        let failure = FailureDecision {
            disposition: FailureDisposition::RetryWithBackoff,
            reason: "transient".to_owned(),
            attempt: 1,
            remaining_attempts: 2,
            backoff_ms: Some(500),
        };
        let mut context = context(&plan, &confidence);
        context.latest_failure = Some(&failure);
        let decision = controller.decide(context).expect("decision");
        assert!(matches!(
            decision.action,
            ContinuationAction::Retry {
                backoff_ms: Some(500),
                ..
            }
        ));
    }

    #[test]
    fn terminal_failure_stops_continuation() {
        let controller =
            ContinuationController::new(ContinuationPolicy::default()).expect("controller");
        let plan = plan(TodoState::InProgress);
        let confidence = confidence(8_000);
        let failure = FailureDecision {
            disposition: FailureDisposition::Terminal,
            reason: "permission denied".to_owned(),
            attempt: 1,
            remaining_attempts: 0,
            backoff_ms: None,
        };
        let mut context = context(&plan, &confidence);
        context.latest_failure = Some(&failure);
        let decision = controller.decide(context).expect("decision");
        assert!(matches!(decision.action, ContinuationAction::Stop { .. }));
    }

    #[test]
    fn stalled_resume_forces_replan() {
        let controller =
            ContinuationController::new(ContinuationPolicy::default()).expect("controller");
        let plan = plan(TodoState::InProgress);
        let confidence = confidence(8_000);
        let mut context = context(&plan, &confidence);
        context.stalled_resumes = 2;
        let decision = controller.decide(context).expect("decision");
        assert!(matches!(decision.action, ContinuationAction::Replan { .. }));
    }

    #[test]
    fn explicit_blocker_is_not_bypassed() {
        let controller =
            ContinuationController::new(ContinuationPolicy::default()).expect("controller");
        let plan = plan(TodoState::Blocked);
        let confidence = confidence(8_000);
        let decision = controller
            .decide(context(&plan, &confidence))
            .expect("decision");
        assert!(matches!(decision.action, ContinuationAction::Block { .. }));
    }

    #[test]
    fn decisions_carry_a_version_and_reject_unknown_actions_or_fields() {
        let controller =
            ContinuationController::new(ContinuationPolicy::default()).expect("controller");
        let plan = plan(TodoState::InProgress);
        let confidence = confidence(8_000);
        let decision = controller
            .decide(context(&plan, &confidence))
            .expect("decision");
        assert_eq!(decision.version, CONTINUATION_ACTION_VERSION);

        // Unknown action tags fail closed.
        let unknown_action = serde_json::json!({
            "version": CONTINUATION_ACTION_VERSION,
            "action": "teleport",
            "details": {},
            "plan_revision": 1,
            "reason": "nope"
        });
        assert!(serde_json::from_value::<ContinuationDecision>(unknown_action).is_err());

        // Unknown fields inside a known action's details fail closed too.
        let unknown_field = serde_json::json!({
            "version": CONTINUATION_ACTION_VERSION,
            "action": "resume",
            "details": {
                "todo_id": "implement",
                "from_checkpoint": true,
                "future_provider_field": 1
            },
            "plan_revision": 1,
            "reason": "nope"
        });
        assert!(serde_json::from_value::<ContinuationDecision>(unknown_field).is_err());

        // The documented shape still round-trips.
        let round_tripped: ContinuationDecision =
            serde_json::from_value(serde_json::to_value(&decision).expect("serialize"))
                .expect("round trip");
        assert_eq!(round_tripped, decision);
    }

    #[test]
    fn exhausted_stall_budget_forces_replan_on_retry_branches() {
        let controller =
            ContinuationController::new(ContinuationPolicy::default()).expect("controller");
        let plan = plan(TodoState::InProgress);
        let confidence = confidence(8_000);
        for disposition in [
            FailureDisposition::RetryImmediately,
            FailureDisposition::RetryWithBackoff,
        ] {
            let failure = FailureDecision {
                disposition,
                reason: "transient".to_owned(),
                attempt: 3,
                remaining_attempts: 0,
                backoff_ms: Some(500),
            };
            let mut stalled = context(&plan, &confidence);
            stalled.latest_failure = Some(&failure);
            stalled.stalled_resumes = 2;
            let decision = controller.decide(stalled).expect("decision");
            assert!(
                matches!(decision.action, ContinuationAction::Replan { .. }),
                "exhausted stall budget must replan, not retry"
            );
        }
    }

    #[test]
    fn plain_in_progress_resume_emits_a_decision_audit_line() {
        let controller =
            ContinuationController::new(ContinuationPolicy::default()).expect("controller");
        let plan = plan(TodoState::InProgress);
        let confidence = confidence(8_000);
        let decision = controller
            .decide(context(&plan, &confidence))
            .expect("decision");
        assert!(matches!(decision.action, ContinuationAction::Resume { .. }));
        let audit = decision.audit_line();
        assert!(audit.starts_with("continuation_decision "));
        assert!(audit.contains("version=1"));
        assert!(audit.contains("plan_revision=1"));
        assert!(audit.contains("action=resume:implement:checkpoint=true"));
        assert!(audit.contains("reason="));
    }
}
