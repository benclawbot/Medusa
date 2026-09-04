//! Production team coordination: bounded multi-agent preflight and
//! worktree-isolated mutating worker execution.

pub mod delegation_contract;
pub(crate) mod multi_agent_coordinator;
#[path = "mutating_worker_coordinator_inner.rs"]
pub(crate) mod mutating_worker_coordinator;
pub mod production_orchestrator;
pub mod team_control;
