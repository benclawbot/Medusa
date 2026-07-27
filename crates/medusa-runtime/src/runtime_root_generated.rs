include!(concat!(env!("OUT_DIR"), "/runtime_generated.rs"));

#[rustfmt::skip]
mod production_orchestrator;

/// Non-production planning metadata retained for future orchestration work.
///
/// The shipped execution path is `RuntimeController -> run_prompt -> AgentEngine`.
/// Nothing in this module dispatches workers or subagents.
pub mod orchestration_planning {
    pub use super::production_orchestrator::*;
}
