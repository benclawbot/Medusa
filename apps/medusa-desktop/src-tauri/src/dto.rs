use medusa_runtime::{AgentPlanStepStatus, RuntimeActivityKind, RuntimeEvent};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStartResponse {
    pub runtime_id: String,
    pub repo: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopCommandSuggestion {
    pub name: String,
    pub usage: String,
    pub description: String,
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
    pub expected_revision: u64,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRecoveryActionRequest {
    pub recovery: serde_json::Value,
    pub operation: String,
    #[serde(default)]
    pub checkpoint_id: Option<String>,
    #[serde(default)]
    pub confirmed_destructive_effects: bool,
    pub repository_fingerprint_before: String,
    pub checkpoint_integrity_verified: bool,
    pub repository_preconditions_verified: bool,
    #[serde(default)]
    pub conflicting_uncommitted_paths: Vec<String>,
    #[serde(default)]
    pub unresolved_risks: Vec<String>,
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
    RecoveryAvailable {
        recovery: serde_json::Value,
    },
    RecoveryCompleted {
        record: serde_json::Value,
        audit_path: String,
    },
    Started,
    AssistantText {
        text: String,
    },
    Activity {
        activity: DesktopActivity,
    },
    Team {
        snapshot: medusa_runtime::TeamSnapshot,
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
    },
    ConfigurationChanged {
        revision: u64,
        active_profile: String,
        changed_keys: Vec<String>,
        origin: String,
        apply_timing: String,
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
    Progress,
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

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DesktopWebArtifact {
    pub path: String,
    pub title: String,
}

impl From<RuntimeEvent> for DesktopRuntimeEvent {
    fn from(event: RuntimeEvent) -> Self {
        match event {
            RuntimeEvent::RecoveryAvailable(recovery) => Self::RecoveryAvailable {
                recovery: serde_json::to_value(recovery).unwrap_or_else(|error| {
                    serde_json::json!({
                        "health": "corrupt",
                        "warnings": [format!("Recovery payload serialization failed: {error}")]
                    })
                }),
            },
            RuntimeEvent::RecoveryCompleted(receipt) => Self::RecoveryCompleted {
                record: serde_json::to_value(receipt.record).unwrap_or_else(|error| {
                    serde_json::json!({
                        "outcome": "failedClosed",
                        "reason": format!("Recovery receipt serialization failed: {error}")
                    })
                }),
                audit_path: receipt.audit_path.to_string_lossy().into_owned(),
            },
            RuntimeEvent::Started => Self::Started,
            RuntimeEvent::AssistantText(text) => Self::AssistantText { text },
            RuntimeEvent::Team(snapshot) => Self::Team { snapshot },
            RuntimeEvent::Activity(activity) => Self::Activity {
                activity: DesktopActivity {
                    id: activity.id,
                    kind: match activity.kind {
                        RuntimeActivityKind::Assistant => DesktopActivityKind::Assistant,
                        RuntimeActivityKind::Done => DesktopActivityKind::Done,
                        RuntimeActivityKind::Error => DesktopActivityKind::Error,
                        RuntimeActivityKind::Tool => DesktopActivityKind::Tool,
                        RuntimeActivityKind::Progress => DesktopActivityKind::Progress,
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
                total_tokens,
                duration_ms,
                tokens_per_second_milli,
                estimated_cost_microusd,
                provenance,
            } => Self::Usage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
                total_tokens,
                duration_ms,
                tokens_per_second_milli,
                estimated_cost_microusd,
                provenance: format!("{provenance:?}").to_ascii_lowercase(),
            },
            RuntimeEvent::Progress { turn } => Self::Progress { turn },
            RuntimeEvent::Settings {
                model,
                effort,
                plan_mode,
                credential_configured,
                ..
            } => Self::Settings {
                model,
                effort,
                plan_mode,
                credential_configured,
            },
            RuntimeEvent::ConfigurationChanged(change) => Self::ConfigurationChanged {
                revision: change.revision,
                active_profile: change.active_profile,
                changed_keys: change.changed_keys,
                origin: change.origin.label().to_owned(),
                apply_timing: change.apply_timing.label().to_owned(),
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
    use medusa_runtime::{
        RuntimeActivity, RuntimePlanStep, TeamSnapshot, TeamWorkerLifecycle, TeamWorkerSnapshot,
    };

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
        let team = serde_json::to_value(DesktopRuntimeEvent::from(RuntimeEvent::Team(
            TeamSnapshot {
                execution_id: Some("execution-1".to_owned()),
                active: true,
                shutdown_requested: false,
                sequence: 7,
                workers: vec![TeamWorkerSnapshot {
                    worker_id: "reviewer-1".to_owned(),
                    role: "reviewer".to_owned(),
                    task_id: "review".to_owned(),
                    lifecycle: TeamWorkerLifecycle::Running,
                    session_id: Some("session-1".to_owned()),
                    turn: 2,
                    last_update: "checking tests".to_owned(),
                    queued_instructions: 1,
                }],
            },
        )))
        .expect("serialize team event");
        assert_eq!(team["type"], "team");
        assert_eq!(team["snapshot"]["executionId"], "execution-1");
        assert_eq!(team["snapshot"]["shutdownRequested"], false);
        assert_eq!(team["snapshot"]["workers"][0]["workerId"], "reviewer-1");
        assert_eq!(team["snapshot"]["workers"][0]["taskId"], "review");
        assert_eq!(team["snapshot"]["workers"][0]["sessionId"], "session-1");
        assert_eq!(
            team["snapshot"]["workers"][0]["lastUpdate"],
            "checking tests"
        );
        assert_eq!(team["snapshot"]["workers"][0]["queuedInstructions"], 1);
        assert!(team["snapshot"].get("execution_id").is_none());

        let usage = serde_json::to_value(DesktopRuntimeEvent::Usage {
            input_tokens: 11,
            output_tokens: 7,
            cache_read_input_tokens: 3,
            cache_creation_input_tokens: 2,
            total_tokens: 23,
            duration_ms: 900,
            tokens_per_second_milli: 25_555,
            estimated_cost_microusd: 123,
            provenance: "providerreported".to_owned(),
        })
        .expect("serialize usage event");
        assert_eq!(usage["inputTokens"], 11);
        assert_eq!(usage["outputTokens"], 7);
        assert_eq!(usage["cacheReadInputTokens"], 3);
        assert_eq!(usage["cacheCreationInputTokens"], 2);
        assert_eq!(usage["totalTokens"], 23);
        assert_eq!(usage["durationMs"], 900);
        assert_eq!(usage["tokensPerSecondMilli"], 25_555);
        assert_eq!(usage["estimatedCostMicrousd"], 123);
        assert_eq!(usage["provenance"], "providerreported");
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
