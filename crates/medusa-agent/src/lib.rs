//! Persistent single-agent execution, role-bound team contexts, and built-in tools.

mod approval;
pub mod branch_summary;
pub mod compaction_v2;
mod engine;
mod engine_support;
mod evidence;
mod identity_guard;
pub mod output_envelope;
mod policy;
mod session;
pub mod session_browser;
pub mod team;
mod tool_dag;
pub mod tools;
mod transaction;
mod verification;
mod verification_authority;
pub mod verification_dag;
mod worker_execution;
pub mod world_model_session;

pub use approval::{
    ApprovalDecision, ApprovalGrant, ApprovalReceipt, ApprovalScope, RollbackOutcome,
    RollbackReceipt,
};
pub use branch_summary::{
    BranchAnchor, BranchSummaryRecord, DeterministicBranchMetadata, capture_restore_abandonment,
    common_ancestor,
};
pub use engine::{AgentEngine, AgentUpdate, StepOutcome};
pub use engine_support::{compact_session, update_session_objective};
pub use identity_guard::{compatibility_context, validate_provider_text};
pub use policy::validate_shell_command;
pub use session::{
    AgentPlanStep, AgentPlanStepStatus, AgentQuestion, AgentQuestionItem, AgentQuestionOption,
    AgentSession, BrowserAssistedLaunch, EscalationJournal, EscalationStatus, SessionEscalation,
    SessionUsage, TurnUsage, UsageProvenance, bootstrap, export_manual_escalation,
    import_manual_advice, launch_browser_assisted_escalation, load_escalation_journal,
    persist_escalation_journal, render_chatgpt_prompt, session_usage,
};
pub use team::{
    AgentExecutionPolicy, TeamMember, TeamMemberContext, TeamMemberLifecycle, TeamRole, TeamRuntime,
};
pub use transaction::{
    FileMutation, TransactionOutcome, TransactionPreview, apply_atomic, preview,
};
pub use verification::VerificationResult;
pub use verification_authority::{
    AuthoritativeVerificationResult, authoritative_verification_for_components,
    authoritative_verification_for_components_at, prepare_components_for_verification,
};
pub use verification_dag::{
    VerificationAuthority, VerificationDag, VerificationInputKey, VerificationNode,
    VerificationNodeState, VerificationReceipt,
};
pub use worker_execution::{
    LeasedAssignment, TeamTaskView, WorkerCompletion, WorkerExecutionController,
    WorkerProgressSummary,
};

/// Appends one canonical session event and commits the resulting snapshot before returning.
pub fn record_session_event(
    session: &mut AgentSession,
    actor: medusa_protocol::Actor,
    payload: medusa_protocol::EventPayload,
) -> medusa_core::MedusaResult<()> {
    evidence::append_event(session, actor, payload)?;
    session.updated_at = time::OffsetDateTime::now_utc();
    session::persist(session)
}
