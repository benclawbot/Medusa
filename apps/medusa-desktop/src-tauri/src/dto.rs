use medusa_runtime::{AgentPlanStepStatus, RuntimeActivityKind, RuntimeEvent};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStartResponse {
    pub runtime_id: String,
    pub repo: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopPromptDraft {
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<DesktopAttachment>,
    #[serde(default)]
    pub revision: u64,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DesktopAttachment {
    File { path: String },
    Image { name: String, data_url: String },
    Text { name: String, text: String },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopModelConfiguration {
    pub provider: String,
    pub model: String,
    pub effort: String,
    #[serde(default)]
    pub api_key: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DesktopSubmitDisposition {
    Started,
    Queued,
}

#[derive(Debug, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum DesktopRuntimeEvent {
    Started,
    AssistantText {
        text: String,
    },
    Activity {
        activity: DesktopActivity,
    },
    Plan {
        steps: Vec<DesktopPlanStep>,
    },
    Question {
        prompts: Vec<DesktopQuestionPrompt>,
    },
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
    Failed {
        message: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopActivity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub kind: DesktopActivityKind,
    pub title: String,
    pub details: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DesktopActivityKind {
    Assistant,
    Done,
    Error,
    Tool,
    Verification,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopPlanStep {
    pub title: String,
    pub status: DesktopPlanStepStatus,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DesktopPlanStepStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopQuestionPrompt {
    pub header: String,
    pub question: String,
    pub options: Vec<DesktopQuestionOption>,
    pub multi_select: bool,
}

#[derive(Debug, Serialize)]
pub struct DesktopQuestionOption {
    pub label: String,
    pub description: String,
}

impl From<RuntimeEvent> for DesktopRuntimeEvent {
    fn from(event: RuntimeEvent) -> Self {
        match event {
            RuntimeEvent::Started => Self::Started,
            RuntimeEvent::AssistantText(text) => Self::AssistantText { text },
            RuntimeEvent::Activity(activity) => Self::Activity {
                activity: DesktopActivity {
                    id: activity.id,
                    kind: match activity.kind {
                        RuntimeActivityKind::Assistant => DesktopActivityKind::Assistant,
                        RuntimeActivityKind::Done => DesktopActivityKind::Done,
                        RuntimeActivityKind::Error => DesktopActivityKind::Error,
                        RuntimeActivityKind::Tool => DesktopActivityKind::Tool,
                        RuntimeActivityKind::Verification => DesktopActivityKind::Verification,
                    },
                    title: activity.title,
                    details: activity.details,
                },
            },
            RuntimeEvent::Plan(steps) => Self::Plan {
                steps: steps
                    .into_iter()
                    .map(|step| DesktopPlanStep {
                        title: step.title,
                        status: match step.status {
                            AgentPlanStepStatus::Pending => DesktopPlanStepStatus::Pending,
                            AgentPlanStepStatus::InProgress => DesktopPlanStepStatus::InProgress,
                            AgentPlanStepStatus::Completed => DesktopPlanStepStatus::Completed,
                            AgentPlanStepStatus::Failed => DesktopPlanStepStatus::Failed,
                        },
                    })
                    .collect(),
            },
            RuntimeEvent::Question(question) => Self::Question {
                prompts: question
                    .prompts()
                    .iter()
                    .map(|prompt| DesktopQuestionPrompt {
                        header: prompt.header.clone(),
                        question: prompt.question.clone(),
                        options: prompt
                            .options
                            .iter()
                            .map(|option| DesktopQuestionOption {
                                label: option.label.clone(),
                                description: option.description.clone(),
                            })
                            .collect(),
                        multi_select: prompt.multi_select,
                    })
                    .collect(),
            },
            RuntimeEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
                model_elapsed_millis,
            } => Self::Usage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
                model_elapsed_millis,
            },
            RuntimeEvent::Progress { turn } => Self::Progress { turn },
            RuntimeEvent::Settings {
                model,
                effort,
                plan_mode,
                credential_configured,
            } => Self::Settings {
                model,
                effort,
                plan_mode,
                credential_configured,
            },
            RuntimeEvent::Notice { title, details } => Self::Notice { title, details },
            RuntimeEvent::NewSession => Self::NewSession,
            RuntimeEvent::Compacted { message } => Self::Compacted { message },
            RuntimeEvent::Completed { session_id } => Self::Completed { session_id },
            RuntimeEvent::TurnFinished => Self::TurnFinished,
            RuntimeEvent::Cancelled => Self::Cancelled,
            RuntimeEvent::Failed(message) => Self::Failed { message },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use medusa_runtime::{RuntimeActivity, RuntimePlanStep};

    #[test]
    fn maps_plan_and_activity_events_without_tui_types() {
        let plan = DesktopRuntimeEvent::from(RuntimeEvent::Plan(vec![RuntimePlanStep {
            title: "Wire desktop".to_owned(),
            status: AgentPlanStepStatus::InProgress,
        }]));
        assert!(
            matches!(plan, DesktopRuntimeEvent::Plan { steps } if matches!(steps[0].status, DesktopPlanStepStatus::InProgress))
        );

        let activity = DesktopRuntimeEvent::from(RuntimeEvent::Activity(RuntimeActivity {
            id: Some("tool-1".to_owned()),
            kind: RuntimeActivityKind::Tool,
            title: "Read file".to_owned(),
            details: Vec::new(),
        }));
        assert!(
            matches!(activity, DesktopRuntimeEvent::Activity { activity } if activity.id.as_deref() == Some("tool-1"))
        );
    }

    #[test]
    fn serializes_runtime_event_fields_for_the_typescript_contract() {
        let usage = serde_json::to_value(DesktopRuntimeEvent::Usage {
            input_tokens: 11,
            output_tokens: 7,
            cache_read_input_tokens: 3,
            cache_creation_input_tokens: 2,
            model_elapsed_millis: 900,
        })
        .expect("serialize usage event");
        assert_eq!(usage["inputTokens"], 11);
        assert_eq!(usage["outputTokens"], 7);
        assert_eq!(usage["cacheReadInputTokens"], 3);
        assert_eq!(usage["cacheCreationInputTokens"], 2);
        assert_eq!(usage["modelElapsedMillis"], 900);
        assert!(usage.get("input_tokens").is_none());

        let settings = serde_json::to_value(DesktopRuntimeEvent::Settings {
            model: "MiniMax-M2.5".to_owned(),
            effort: "effort:auto".to_owned(),
            plan_mode: false,
            credential_configured: true,
        })
        .expect("serialize settings event");
        assert_eq!(settings["planMode"], false);
        assert_eq!(settings["credentialConfigured"], true);
        assert!(settings.get("plan_mode").is_none());
    }
}
