//! Persistent single-agent orchestration and built-in tools.

mod approval;
mod engine;
mod engine_support;
mod evidence;
mod identity_guard;
pub mod output_envelope;
mod policy;
mod runtime_failure;
mod session;
pub mod session_browser;
pub mod tools;
mod transaction;
mod verification;
mod worker_execution;
pub mod world_model_session;

pub use approval::{
    ApprovalDecision, ApprovalGrant, ApprovalReceipt, ApprovalScope, RollbackOutcome,
    RollbackReceipt,
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
pub use transaction::{
    FileMutation, TransactionOutcome, TransactionPreview, apply_atomic, preview,
};
pub use verification::{VerificationResult, targeted_verification};
pub use worker_execution::{
    LeasedAssignment, WorkerCompletion, WorkerExecutionController, WorkerProgressSummary,
};

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        fs,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::Duration,
    };

    #[cfg(target_os = "linux")]
    use std::process::Command;

    use medusa_config::{Config, Mode};
    use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
    use medusa_protocol::EventPayload;
    use medusa_provider::{ModelProvider, ModelRequest, ModelResponse, ResponseBlock, Usage};
    use serde_json::json;

    use super::*;
    use crate::{
        policy::safe_path,
        tools::{execute_approved_tool, execute_tool},
    };

    struct ScriptedProvider {
        responses: Mutex<VecDeque<ModelResponse>>,
    }

    impl ScriptedProvider {
        fn new(responses: Vec<ModelResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
            }
        }
    }
