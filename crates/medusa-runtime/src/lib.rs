use std::{
    collections::{BTreeMap, VecDeque},
    env,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread,
};

use medusa_agent::{
    AgentEngine, AgentPlanStep, AgentQuestion, AgentSession, StepOutcome, compact_session,
    update_session_objective,
};
use medusa_capabilities::CapabilityRegistry;
use medusa_config::{Config, Mode};
use medusa_provider::{ConfiguredProvider, ModelProvider};

use crate::{
    commands::{Effort, ModelCommand, ModelConfiguration, SlashCommand},
    prompt::PromptDraft,
};

pub mod commands;
mod error;
pub mod prompt;
pub mod skill_dependencies;
pub mod skill_dependency_locks;
mod support;
#[cfg(test)]
mod tests;

pub use error::RuntimeError;
pub use medusa_agent::{
    AgentPlanStep as RuntimePlanStep, AgentPlanStepStatus, AgentQuestionItem, AgentQuestionOption,
};

use support::{
    SelectedSkill, UpdateState, configure_model, credential_environment, discover_skills,
    effort_for_turns, forward_update, is_supported_provider, load_selected_skill, message_blocks,
    model_configuration_details, objective_for, should_auto_compact, turns_for_effort,
};

#[derive(Debug)]
pub enum RuntimeCommand {
    Submit(PromptDraft),
    Slash(SlashCommand),
    ConfigureModel(ModelConfiguration),
    Shutdown,
}

#[derive(Debug)]
pub enum RuntimeEvent {
    Started,
    AssistantText(String),
    Activity(RuntimeActivity),
    Plan(Vec<AgentPlanStep>),
    Question(AgentQuestion),
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        cache_read_input_tokens: u64,
        cache_creation_input_tokens: u64,
        model_elapsed_millis: u64,
    },
    Progress {
        turn: u32,
    },
    Settings {
        model: String,
        effort: String,
        plan_mode: bool,
        credential_configured: bool,
        context_window_tokens: u64,
        auto_compact_percent: u8,
    },
    Notice {
        title: String,
        details: Vec<String>,
    },
    Error(String),
    Completed,
}
