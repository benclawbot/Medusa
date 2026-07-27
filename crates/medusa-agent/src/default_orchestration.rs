//! Default production orchestration policy for repository coding sessions.
//!
//! This module is intentionally policy-only. Runtime wiring consumes this
//! decision and roster so the TUI, desktop application, and headless CLI use
//! the same orchestration boundary.

use crate::session::AgentSession;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OrchestrationMode {
    /// Conversational, clarification-only, or genuinely atomic work stays on
    /// the coordinator to avoid unnecessary worker and transaction overhead.
    Coordinator,
    /// Repository work with multiple visible plan steps uses durable workers,
    /// leases, review, transaction integration, and verification.
    MultiAgent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProductionWorkerRole {
    Researcher,
    Coder,
    Reviewer,
    Tester,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProductionWorker {
    pub id: String,
    pub role: ProductionWorkerRole,
    pub capacity: u16,
}

/// Select the default runtime path from durable session state rather than a
/// user-facing experimental flag.
#[must_use]
pub(crate) fn mode_for(session: &AgentSession) -> OrchestrationMode {
    if session.completed || session.pending_question.is_some() || session.plan.len() < 2 {
        OrchestrationMode::Coordinator
    } else {
        OrchestrationMode::MultiAgent
    }
}

/// Build the production worker roster. Concurrency is bounded by the existing
/// typed `agent.parallel_workers` setting, but review remains independent so a
/// coding worker never approves its own transaction.
#[must_use]
pub(crate) fn production_workers(parallel_workers: u16) -> Vec<ProductionWorker> {
    let coding_capacity = parallel_workers.clamp(1, 8);
    vec![
        ProductionWorker {
            id: "researcher-1".to_owned(),
            role: ProductionWorkerRole::Researcher,
            capacity: 1,
        },
        ProductionWorker {
            id: "coder-1".to_owned(),
            role: ProductionWorkerRole::Coder,
            capacity: coding_capacity,
        },
        ProductionWorker {
            id: "reviewer-1".to_owned(),
            role: ProductionWorkerRole::Reviewer,
            capacity: 1,
        },
        ProductionWorker {
            id: "tester-1".to_owned(),
            role: ProductionWorkerRole::Tester,
            capacity: 1,
        },
    ]
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use medusa_core::SessionId;
    use time::OffsetDateTime;

    use super::*;
    use crate::session::{AgentPlanStep, AgentPlanStepStatus};

    fn session_with_plan(steps: usize) -> AgentSession {
        AgentSession {
            id: SessionId::new(),
            objective: "implement and verify the change".to_owned(),
            repo: PathBuf::from("."),
            created_at: OffsetDateTime::now_utc(),
            updated_at: OffsetDateTime::now_utc(),
            completed: false,
            turn: 0,
            messages: Vec::new(),
            plan: (0..steps)
                .map(|index| AgentPlanStep {
                    title: format!("Step {}", index + 1),
                    status: AgentPlanStepStatus::Pending,
                })
                .collect(),
            evidence: Vec::new(),
            events: Vec::new(),
            repo_fingerprint: String::new(),
            tool_artifacts: Vec::new(),
            pending_question: None,
            browser_assisted_launch: None,
            approval_grants: Vec::new(),
            approval_receipts: Vec::new(),
            rollback_receipts: Vec::new(),
            world_model: None,
        }
    }

    #[test]
    fn multi_step_repository_work_uses_multi_agent_runtime() {
        assert_eq!(mode_for(&session_with_plan(3)), OrchestrationMode::MultiAgent);
    }

    #[test]
    fn atomic_work_stays_on_coordinator() {
        assert_eq!(mode_for(&session_with_plan(1)), OrchestrationMode::Coordinator);
    }

    #[test]
    fn production_roster_has_independent_review_and_bounded_coding_capacity() {
        let workers = production_workers(64);
        assert_eq!(workers.len(), 4);
        assert_eq!(workers[1].capacity, 8);
        assert_eq!(workers[2].role, ProductionWorkerRole::Reviewer);
        assert_ne!(workers[1].id, workers[2].id);
    }
}
