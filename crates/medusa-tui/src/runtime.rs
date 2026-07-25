use std::path::PathBuf;

use crate::app::{
    QuestionOption, QuestionPrompt, TranscriptPlan, TranscriptPlanStep, TranscriptPlanStepState,
};
use crate::clipboard::PromptDraft;
use crate::commands::{ModelConfiguration, SlashCommand};

pub use medusa_runtime::{RuntimeActivity, RuntimeActivityKind, RuntimeError, SubmitDisposition};

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

fn presentation_activity(mut activity: RuntimeActivity) -> RuntimeActivity {
    let (kind, label) = match activity.kind {
        RuntimeActivityKind::Assistant if !activity.details.is_empty() => {
            (RuntimeActivityKind::Progress, Some("Assistant"))
        }
        RuntimeActivityKind::Tool if !activity.details.is_empty() => {
            (RuntimeActivityKind::Verification, Some("Tool"))
        }
        _ => (activity.kind, None),
    };
    activity.kind = kind;
    if let Some(label) = label {
        activity.title = format!("{label} · {}", activity.title);
    }
    activity
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_all_agent_plan_statuses_into_terminal_presentation_states() {
        let event = medusa_runtime::RuntimeEvent::Plan(vec![
            medusa_runtime::RuntimePlanStep {
                title: "Pending".to_owned(),
                status: medusa_runtime::AgentPlanStepStatus::Pending,
            },
            medusa_runtime::RuntimePlanStep {
                title: "Active".to_owned(),
                status: medusa_runtime::AgentPlanStepStatus::InProgress,
            },
            medusa_runtime::RuntimePlanStep {
                title: "Done".to_owned(),
                status: medusa_runtime::AgentPlanStepStatus::Completed,
            },
            medusa_runtime::RuntimePlanStep {
                title: "Failed".to_owned(),
                status: medusa_runtime::AgentPlanStepStatus::Failed,
            },
        ]);
        let RuntimeEvent::Plan(plan) = map_event(event) else {
            panic!("expected plan event");
        };
        assert_eq!(
            plan.steps.iter().map(|step| step.state).collect::<Vec<_>>(),
            vec![
                TranscriptPlanStepState::Pending,
                TranscriptPlanStepState::Active,
                TranscriptPlanStepState::Completed,
                TranscriptPlanStepState::Failed,
            ]
        );
    }

    #[test]
    fn detailed_tool_activity_uses_a_detail_capable_presentation_kind() {
        let activity = presentation_activity(RuntimeActivity {
            id: Some("tool-1".to_owned()),
            kind: RuntimeActivityKind::Tool,
            title: "Read render.rs".to_owned(),
            details: vec!["crates/medusa-tui/src/render.rs".to_owned()],
        });
        assert_eq!(activity.kind, RuntimeActivityKind::Verification);
        assert_eq!(activity.title, "Tool · Read render.rs");
        assert_eq!(activity.details.len(), 1);
    }

    #[test]
    fn compact_tool_activity_keeps_its_original_kind() {
        let activity = presentation_activity(RuntimeActivity {
            id: Some("tool-2".to_owned()),
            kind: RuntimeActivityKind::Tool,
            title: "Thinking".to_owned(),
            details: Vec::new(),
        });
        assert_eq!(activity.kind, RuntimeActivityKind::Tool);
        assert_eq!(activity.title, "Thinking");
    }
}
