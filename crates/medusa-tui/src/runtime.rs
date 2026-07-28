use std::path::PathBuf;

use crate::app::{
    QuestionOption, QuestionPrompt, TranscriptPlan, TranscriptPlanStep, TranscriptPlanStepState,
};
use crate::clipboard::PromptDraft;
use crate::commands::{ModelConfiguration, SlashCommand};

pub use medusa_runtime::{
    RecoveryActionRequest, RecoveryOperation, RecoveryPreflightEvidence, RecoveryView,
    RuntimeActivity, RuntimeActivityKind, RuntimeError, SubmitDisposition,
};

#[derive(Debug)]
pub enum RuntimeEvent {
    Started,
    AssistantText(String),
    Activity(RuntimeActivity),
    Plan(TranscriptPlan),
    Question(RuntimeQuestion),
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        cache_read_input_tokens: u64,
        cache_creation_input_tokens: u64,
        total_tokens: u64,
        duration_ms: u64,
        tokens_per_second_milli: u64,
        estimated_cost_microusd: u64,
        provenance: String,
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
    NewSession,
    Compacted {
        message: String,
    },
    Completed {
        session_id: String,
    },
    TurnFinished,
    Cancelled,
    Failed(String),
}

#[derive(Debug)]
pub struct RuntimeQuestion {
    pub questions: Vec<QuestionPrompt>,
}

pub struct RuntimeController {
    inner: medusa_runtime::RuntimeController,
}

impl RuntimeController {
    pub fn start(repo: PathBuf) -> Self {
        Self {
            inner: medusa_runtime::RuntimeController::start(repo),
        }
    }
    pub fn submit(&self, draft: PromptDraft) -> Result<SubmitDisposition, RuntimeError> {
        self.inner.submit(draft)
    }
    pub fn run_command(&self, command: SlashCommand) -> Result<(), RuntimeError> {
        self.inner.run_command(command)
    }
    pub fn configure_model(&self, configuration: ModelConfiguration) -> Result<(), RuntimeError> {
        self.inner.configure_model(configuration)
    }
    pub fn execute_recovery(
        &self,
        view: RecoveryView,
        request: RecoveryActionRequest,
        preflight: RecoveryPreflightEvidence,
    ) -> Result<(), RuntimeError> {
        self.inner.execute_recovery(view, request, preflight)
    }
    pub fn cancel(&self) -> bool {
        self.inner.cancel()
    }
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.inner.is_busy()
    }
    pub fn try_event(&self) -> Result<Option<RuntimeEvent>, RuntimeError> {
        self.inner.try_event().map(|event| event.map(map_event))
    }
}

fn map_event(event: medusa_runtime::RuntimeEvent) -> RuntimeEvent {
    match event {
        medusa_runtime::RuntimeEvent::RecoveryAvailable(view) => RuntimeEvent::Notice {
            title: "Recovery available".to_owned(),
            details: recovery_details(&view),
        },
        medusa_runtime::RuntimeEvent::RecoveryCompleted(receipt) => RuntimeEvent::Notice {
            title: "Recovery action recorded".to_owned(),
            details: vec![
                format!("Session: {}", receipt.record.session_id),
                format!("Action: {:?}", receipt.record.operation),
                format!("Outcome: {:?}", receipt.record.outcome),
                format!("Audit: {}", receipt.audit_path.display()),
            ],
        },
        medusa_runtime::RuntimeEvent::Started => RuntimeEvent::Started,
        medusa_runtime::RuntimeEvent::AssistantText(text) => RuntimeEvent::AssistantText(text),
        medusa_runtime::RuntimeEvent::Activity(activity) => {
            RuntimeEvent::Activity(presentation_activity(activity))
        }
        medusa_runtime::RuntimeEvent::Plan(steps) => RuntimeEvent::Plan(TranscriptPlan {
            steps: steps
                .into_iter()
                .map(|step| TranscriptPlanStep {
                    title: step.title,
                    state: match step.status {
                        medusa_runtime::AgentPlanStepStatus::Pending => {
                            TranscriptPlanStepState::Pending
                        }
                        medusa_runtime::AgentPlanStepStatus::InProgress => {
                            TranscriptPlanStepState::Active
                        }
                        medusa_runtime::AgentPlanStepStatus::Completed => {
                            TranscriptPlanStepState::Completed
                        }
                        medusa_runtime::AgentPlanStepStatus::Failed => {
                            TranscriptPlanStepState::Failed
                        }
                    },
                })
                .collect(),
        }),
        medusa_runtime::RuntimeEvent::Question(question) => {
            RuntimeEvent::Question(RuntimeQuestion {
                questions: question
                    .prompts()
                    .iter()
                    .map(|item| QuestionPrompt {
                        header: item.header.clone(),
                        question: item.question.clone(),
                        options: item
                            .options
                            .iter()
                            .map(|option| QuestionOption {
                                label: option.label.clone(),
                                description: option.description.clone(),
                            })
                            .collect(),
                        multi_select: item.multi_select,
                    })
                    .collect(),
            })
        }
        medusa_runtime::RuntimeEvent::Usage {
            input_tokens,
            output_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
            total_tokens,
            duration_ms,
            tokens_per_second_milli,
            estimated_cost_microusd,
            provenance,
        } => RuntimeEvent::Usage {
            input_tokens,
            output_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
            total_tokens,
            duration_ms,
            tokens_per_second_milli,
            estimated_cost_microusd,
            provenance: match provenance {
                medusa_runtime::UsageProvenance::ProviderReported => "provider".to_owned(),
                medusa_runtime::UsageProvenance::Estimated => "estimated".to_owned(),
            },
        },
        medusa_runtime::RuntimeEvent::Progress { turn } => RuntimeEvent::Progress { turn },
        medusa_runtime::RuntimeEvent::Settings {
            model,
            effort,
            plan_mode,
            credential_configured,
            context_window_tokens,
            auto_compact_percent,
        } => RuntimeEvent::Settings {
            model,
            effort,
            plan_mode,
            credential_configured,
            context_window_tokens,
            auto_compact_percent,
        },
        medusa_runtime::RuntimeEvent::Notice { title, details } => {
            RuntimeEvent::Notice { title, details }
        }
        medusa_runtime::RuntimeEvent::NewSession => RuntimeEvent::NewSession,
        medusa_runtime::RuntimeEvent::Compacted { message } => RuntimeEvent::Compacted { message },
        medusa_runtime::RuntimeEvent::Completed { session_id } => {
            RuntimeEvent::Completed { session_id }
        }
        medusa_runtime::RuntimeEvent::TurnFinished => RuntimeEvent::TurnFinished,
        medusa_runtime::RuntimeEvent::Cancelled => RuntimeEvent::Cancelled,
        medusa_runtime::RuntimeEvent::Failed(error) => RuntimeEvent::Failed(error),
    }
}

fn recovery_details(view: &medusa_recovery_coordinator::RecoveryView) -> Vec<String> {
    let mut details = vec![
        format!("Session: {}", view.session_id),
        format!("Last durable step: {}", view.last_durable_step),
        format!("Health: {:?}", view.health),
        format!("Checkpoints: {}", view.checkpoints.len()),
    ];
    if let Some(operation) = &view.interrupted_operation {
        details.push(format!("Interrupted: {operation}"));
    }
    details.extend(view.warnings.iter().cloned());
    details
}

fn presentation_activity(mut activity: RuntimeActivity) -> RuntimeActivity {
    activity.details.retain(|detail| !detail.trim().is_empty());
    let (kind, label) = match activity.kind {
        RuntimeActivityKind::Assistant if !activity.details.is_empty() => {
            (RuntimeActivityKind::Progress, Some("Assistant"))
        }
        RuntimeActivityKind::Tool if !activity.details.is_empty() => (
            RuntimeActivityKind::Verification,
            Some(tool_activity_label(&activity.title)),
        ),
        _ => (activity.kind, None),
    };
    activity.kind = kind;
    if let Some(label) = label {
        activity.title = format!("{label} · {}", activity.title);
    }
    activity
}

fn tool_activity_label(title: &str) -> &'static str {
    let title = title.to_ascii_lowercase();
    if ["test", "check", "verify", "lint", "clippy"]
        .iter()
        .any(|keyword| title.contains(keyword))
    {
        "Test"
    } else if ["edit", "write", "patch", "update", "create", "delete"]
        .iter()
        .any(|keyword| title.contains(keyword))
    {
        "Edit"
    } else if ["run", "shell", "command", "build", "cargo", "npm"]
        .iter()
        .any(|keyword| title.contains(keyword))
    {
        "Run"
    } else if ["fetch", "download", "http", "web", "request"]
        .iter()
        .any(|keyword| title.contains(keyword))
    {
        "Fetch"
    } else if ["read", "search", "find", "list", "inspect", "open"]
        .iter()
        .any(|keyword| title.contains(keyword))
    {
        "Read"
    } else {
        "Tool"
    }
}
