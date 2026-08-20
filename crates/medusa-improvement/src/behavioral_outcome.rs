//! Versioned behavioral outcome contract projected from Medusa's durable session journal.
//!
//! Journal events, verification receipts, and integration receipts remain authoritative. This
//! module only reduces those sources into a deterministic per-task behavioral record. Model,
//! worker, or reviewer prose never establishes correctness.

use std::collections::BTreeMap;

use medusa_protocol::{Actor, EventEnvelope, EventPayload};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const BEHAVIORAL_OUTCOME_SCHEMA_VERSION: u16 = 1;
pub const TASK_CLASSIFICATION_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BehavioralTerminalStatus {
    VerifiedSuccess,
    VerifiedFailure,
    Cancelled,
    Partial,
    Inconclusive,
    Invalidated,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BehavioralWorkspaceMode {
    Git,
    Directory,
    Ephemeral,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BehavioralTaskIntent {
    ReadOnlyAnalysis,
    BugFix,
    Feature,
    Refactor,
    DependencyMigration,
    TestRepair,
    UiBrowser,
    Documentation,
    RecoveryResume,
    Other,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BehavioralRiskClass {
    ReadOnly,
    Low,
    Medium,
    High,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BehavioralComplexityBand {
    Small,
    Medium,
    Large,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BehavioralTaskClassificationV1 {
    pub schema_version: u16,
    pub workspace_mode: BehavioralWorkspaceMode,
    pub intent: BehavioralTaskIntent,
    pub language_families: Vec<String>,
    pub risk_class: BehavioralRiskClass,
    pub complexity_band: BehavioralComplexityBand,
    pub task_features: Vec<String>,
    pub unknowns: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BehavioralModelExecutionV1 {
    pub event_id: String,
    pub event_sequence: u64,
    pub provider: String,
    pub model: String,
    pub request_id: Option<String>,
    pub request_fingerprint: Option<String>,
    pub manifest_ref: Option<String>,
    pub attempt_ordinal: u32,
    pub parent_request_id: Option<String>,
    pub response_id: Option<String>,
    pub usage: Option<Value>,
    pub failed: bool,
    pub failure_event_id: Option<String>,
    pub mutation_contribution: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BehavioralToolExecutionV1 {
    pub tool: String,
    pub requested_event_id: String,
    pub requested_sequence: u64,
    pub denied: bool,
    pub completed: bool,
    pub exit_code: Option<i32>,
    pub queue_duration_ns: Option<u64>,
    pub execution_duration_ns: Option<u64>,
    pub cached: Option<bool>,
    pub source_event_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BehavioralOutcomeV1 {
    pub schema_version: u16,
    pub outcome_id: String,
    pub root_task_id: String,
    pub session_id: String,
    pub trajectory_id: String,
    pub root_task_eligible: bool,
    pub repository_revision: Option<String>,
    pub harness_version: String,
    pub task_classification: BehavioralTaskClassificationV1,
    pub terminal_status: BehavioralTerminalStatus,
    pub verified_success: bool,
    pub verification_passed: Option<bool>,
    pub verification_receipt_ids: Vec<String>,
    pub integration_receipt_ids: Vec<String>,
    pub model_executions: Vec<BehavioralModelExecutionV1>,
    pub provider_execution_records: Vec<Value>,
    pub tool_executions: Vec<BehavioralToolExecutionV1>,
    pub mutation_count: u32,
    pub verification_attempts: u32,
    pub failed_verification_attempts: u32,
    pub recovery_count: u32,
    pub cancellation_requested: bool,
    pub user_correction_count: u32,
    pub approval_denial_count: u32,
    pub latency_millis: Option<u64>,
    pub observed_token_usage: Option<u64>,
    pub monetary_cost_microunits: Option<u64>,
    pub source_event_ids: Vec<String>,
    pub source_event_checksums: Vec<String>,
    pub first_event_unix_ms: Option<i64>,
    pub last_event_unix_ms: Option<i64>,
}

impl BehavioralOutcomeV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != BEHAVIORAL_OUTCOME_SCHEMA_VERSION {
            return Err("unsupported behavioral outcome schema version");
        }
        if self.task_classification.schema_version != TASK_CLASSIFICATION_SCHEMA_VERSION {
            return Err("unsupported task classification schema version");
        }
        if self.outcome_id.trim().is_empty()
            || self.root_task_id.trim().is_empty()
            || self.session_id.trim().is_empty()
            || self.trajectory_id.trim().is_empty()
            || self.source_event_ids.is_empty()
            || self.source_event_ids.len() != self.source_event_checksums.len()
        {
            return Err("behavioral outcome is incomplete");
        }
        if self.verified_success
            && (!self.root_task_eligible
                || self.terminal_status != BehavioralTerminalStatus::VerifiedSuccess
                || self.verification_passed == Some(false)
                || self.verification_receipt_ids.is_empty()
                    && self.integration_receipt_ids.is_empty())
        {
            return Err(
                "verified behavioral success requires eligible root verification authority",
            );
        }
        Ok(())
    }

    #[must_use]
    pub fn contributing_execution(&self) -> Option<&BehavioralModelExecutionV1> {
        self.model_executions
            .iter()
            .rev()
            .find(|execution| execution.mutation_contribution)
    }
}

pub fn project_behavioral_outcome(
    session_id: &str,
    repository_revision: Option<String>,
    harness_version: String,
    events: &[EventEnvelope],
) -> Result<BehavioralOutcomeV1, String> {
    if events.is_empty() {
        return Err("behavioral outcome requires durable journal events".to_owned());
    }

    for event in events {
        event.validate().map_err(|error| error.to_string())?;
    }
    for pair in events.windows(2) {
        if pair[1].sequence != pair[0].sequence.saturating_add(1)
            || pair[1].previous_hash.as_deref() != Some(pair[0].checksum.as_str())
        {
            return Err("behavioral outcome source event chain is discontinuous".to_owned());
        }
    }

    // A completed session can later receive lifecycle events such as SessionReset. Keep the
    // outcome projection tied to the first terminal boundary so replay after that boundary does
    // not rewrite the completed task's evidence or outcome identity.
    let terminal_len = events
        .iter()
        .position(|event| {
            matches!(
                &event.payload,
                EventPayload::SessionCompleted { .. }
                    | EventPayload::SessionFailed { .. }
                    | EventPayload::CancellationCompleted
            )
        })
        .map_or(events.len(), |index| index + 1);
    let events = &events[..terminal_len];

    let source_event_ids = events.iter().map(event_id).collect::<Vec<_>>();
    let source_event_checksums = events
        .iter()
        .map(|event| event.checksum.clone())
        .collect::<Vec<_>>();
    let first_event_unix_ms = events.first().map(event_unix_ms);
    let last_event_unix_ms = events.last().map(event_unix_ms);
    let latency_millis = first_event_unix_ms
        .zip(last_event_unix_ms)
        .and_then(|(first, last)| u64::try_from(last.saturating_sub(first)).ok());

    let root_task_eligible = !delegated_worker_events(events);
    let mut model_executions = Vec::<BehavioralModelExecutionV1>::new();
    let mut request_indexes = BTreeMap::<String, usize>::new();
    let mut latest_successful_response = None::<usize>;
    let mut provider_execution_records = Vec::new();
    let mut tools = Vec::<BehavioralToolExecutionV1>::new();
    let mut active_tool: Option<usize> = None;
    let mut verification_passed = None;
    let mut verification_receipt_ids = Vec::new();
    let mut integration_receipt_ids = Vec::new();
    let mut mutation_count = 0u32;
    let mut verification_attempts = 0u32;
    let mut failed_verification_attempts = 0u32;
    let mut recovery_count = 0u32;
    let mut cancellation_requested = false;
    let mut cancellation_completed = false;
    let mut session_completed = false;
    let mut session_failed = false;
    let mut runtime_failed = false;
    let mut user_correction_count = 0u32;
    let mut approval_denial_count = 0u32;
    let mut objective = String::new();
    let mut changed_paths = Vec::new();

    for event in events {
        match &event.payload {
            EventPayload::SessionCreated { objective: value } => objective = value.clone(),
            EventPayload::ModelRequestStarted {
                provider,
                model,
                request_id,
                request_fingerprint,
                manifest_ref,
                attempt_ordinal,
                parent_request_id,
            } => {
                let index = model_executions.len();
                if let Some(request_id) = request_id.as_ref() {
                    request_indexes.insert(request_id.clone(), index);
                }
                model_executions.push(BehavioralModelExecutionV1 {
                    event_id: event_id(event),
                    event_sequence: event.sequence,
                    provider: provider.clone(),
                    model: model.clone(),
                    request_id: request_id.clone(),
                    request_fingerprint: request_fingerprint.clone(),
                    manifest_ref: manifest_ref.clone(),
                    attempt_ordinal: *attempt_ordinal,
                    parent_request_id: parent_request_id.clone(),
                    response_id: None,
                    usage: None,
                    failed: false,
                    failure_event_id: None,
                    mutation_contribution: false,
                });
            }
            EventPayload::ModelResponseReceived {
                response_id,
                usage,
                request_id,
                ..
            } => {
                if let Some(index) = model_execution_index(
                    request_id.as_deref(),
                    &request_indexes,
                    &model_executions,
                ) {
                    model_executions[index].response_id = response_id.clone();
                    model_executions[index].usage = Some(usage.clone());
                    latest_successful_response = Some(index);
                }
            }
            EventPayload::ModelRequestFailed { request_id, .. } => {
                if let Some(index) = request_indexes.get(request_id).copied() {
                    model_executions[index].failed = true;
                    model_executions[index].failure_event_id = Some(event_id(event));
                    if latest_successful_response == Some(index) {
                        latest_successful_response = None;
                    }
                }
            }
            EventPayload::ProviderExecutionRecorded { status } => {
                provider_execution_records.push(status.clone());
            }
            EventPayload::ToolCallRequested { tool, .. } => {
                tools.push(BehavioralToolExecutionV1 {
                    tool: tool.clone(),
                    requested_event_id: event_id(event),
                    requested_sequence: event.sequence,
                    denied: false,
                    completed: false,
                    exit_code: None,
                    queue_duration_ns: None,
                    execution_duration_ns: None,
                    cached: None,
                    source_event_ids: vec![event_id(event)],
                });
                active_tool = Some(tools.len() - 1);
            }
            EventPayload::ToolCallDenied { tool, .. } => {
                if let Some(index) = find_tool(&tools, active_tool, tool) {
                    tools[index].denied = true;
                    tools[index].source_event_ids.push(event_id(event));
                }
            }
            EventPayload::ToolExecutionCompleted { tool, exit_code } => {
                if let Some(index) = find_tool(&tools, active_tool, tool) {
                    tools[index].completed = true;
                    tools[index].exit_code = *exit_code;
                    tools[index].source_event_ids.push(event_id(event));
                }
            }
            EventPayload::ToolExecutionTimingRecorded {
                tool,
                queue_duration_ns,
                execution_duration_ns,
                cached,
                ..
            } => {
                if let Some(index) = find_tool(&tools, active_tool, tool) {
                    tools[index].queue_duration_ns = Some(*queue_duration_ns);
                    tools[index].execution_duration_ns = Some(*execution_duration_ns);
                    tools[index].cached = Some(*cached);
                    tools[index].source_event_ids.push(event_id(event));
                }
            }
            EventPayload::FileTransactionCommitted { paths, .. } => {
                mutation_count = mutation_count.saturating_add(1);
                changed_paths.extend(paths.iter().cloned());
                if let Some(index) = latest_successful_response {
                    model_executions[index].mutation_contribution = true;
                }
            }
            EventPayload::VerificationStarted { .. } => {
                verification_attempts = verification_attempts.saturating_add(1);
            }
            EventPayload::VerificationCompleted { passed, evidence } => {
                verification_passed = Some(*passed);
                if !passed {
                    failed_verification_attempts = failed_verification_attempts.saturating_add(1);
                }
                if evidence.is_empty() {
                    verification_receipt_ids.push(event_id(event));
                } else {
                    verification_receipt_ids.extend(evidence.iter().cloned());
                }
            }
            EventPayload::IntegrationReceiptRecorded { receipt } => {
                integration_receipt_ids
                    .push(receipt_id(receipt).unwrap_or_else(|| event_id(event)));
            }
            EventPayload::RecoveryActionCompleted { .. } => {
                recovery_count = recovery_count.saturating_add(1);
            }
            EventPayload::CancellationRequested { .. } => cancellation_requested = true,
            EventPayload::CancellationCompleted => cancellation_completed = true,
            EventPayload::UserFollowupQueued { .. }
            | EventPayload::UserPromptReceived { .. }
            | EventPayload::GoalUpdated { .. } => {
                user_correction_count = user_correction_count.saturating_add(1);
            }
            EventPayload::ApprovalDecisionRecorded { decision } => {
                if decision_is_denial(decision) {
                    approval_denial_count = approval_denial_count.saturating_add(1);
                }
            }
            EventPayload::RuntimeFailed { .. } => runtime_failed = true,
            EventPayload::SessionFailed { .. } => session_failed = true,
            EventPayload::SessionCompleted { .. } => session_completed = true,
            _ => {}
        }
    }

    verification_receipt_ids.sort();
    verification_receipt_ids.dedup();
    integration_receipt_ids.sort();
    integration_receipt_ids.dedup();

    let verification_authority_sequence = if verification_passed == Some(true) {
        events.iter().rev().find_map(|event| {
            matches!(
                &event.payload,
                EventPayload::VerificationCompleted { passed: true, .. }
            )
            .then_some(event.sequence)
        })
    } else if verification_passed.is_none() {
        events.iter().rev().find_map(|event| {
            matches!(
                &event.payload,
                EventPayload::IntegrationReceiptRecorded { .. }
            )
            .then_some(event.sequence)
        })
    } else {
        None
    };
    let invalidated_after_verification = verification_authority_sequence.is_some_and(|sequence| {
        events.iter().any(|event| {
            event.sequence >= sequence
                && matches!(
                    &event.payload,
                    EventPayload::SessionFailed { .. }
                        | EventPayload::RuntimeFailed { .. }
                        | EventPayload::CancellationCompleted
                )
        })
    });

    let root_verification_passed = verification_passed == Some(true)
        || (verification_passed.is_none() && !integration_receipt_ids.is_empty());
    let verified_success = root_task_eligible
        && session_completed
        && root_verification_passed
        && (!verification_receipt_ids.is_empty() || !integration_receipt_ids.is_empty())
        && !invalidated_after_verification;
    let terminal_status = if verified_success {
        BehavioralTerminalStatus::VerifiedSuccess
    } else if cancellation_completed {
        BehavioralTerminalStatus::Cancelled
    } else if verification_passed == Some(false) {
        BehavioralTerminalStatus::VerifiedFailure
    } else if mutation_count > 0 && (session_failed || runtime_failed) {
        BehavioralTerminalStatus::Partial
    } else if session_failed || runtime_failed {
        BehavioralTerminalStatus::Invalidated
    } else {
        BehavioralTerminalStatus::Inconclusive
    };

    let observed_token_usage = observed_token_usage(&model_executions);
    let monetary_cost_microunits = observed_monetary_cost(&provider_execution_records);
    let task_classification = classify_task(
        &objective,
        &changed_paths,
        events.len(),
        repository_revision.as_ref(),
        mutation_count,
        &tools,
    );
    let mut outcome = BehavioralOutcomeV1 {
        schema_version: BEHAVIORAL_OUTCOME_SCHEMA_VERSION,
        outcome_id: String::new(),
        root_task_id: session_id.to_owned(),
        session_id: session_id.to_owned(),
        trajectory_id: session_id.to_owned(),
        root_task_eligible,
        repository_revision,
        harness_version,
        task_classification,
        terminal_status,
        verified_success,
        verification_passed,
        verification_receipt_ids,
        integration_receipt_ids,
        model_executions,
        provider_execution_records,
        tool_executions: tools,
        mutation_count,
        verification_attempts,
        failed_verification_attempts,
        recovery_count,
        cancellation_requested,
        user_correction_count,
        approval_denial_count,
        latency_millis,
        observed_token_usage,
        monetary_cost_microunits,
        source_event_ids,
        source_event_checksums,
        first_event_unix_ms,
        last_event_unix_ms,
    };
    outcome.outcome_id = normalized_outcome_id(&outcome)?;
    outcome.validate().map_err(str::to_owned)?;
    Ok(outcome)
}

fn classify_task(
    objective: &str,
    changed_paths: &[String],
    event_count: usize,
    repository_revision: Option<&String>,
    mutation_count: u32,
    tools: &[BehavioralToolExecutionV1],
) -> BehavioralTaskClassificationV1 {
    let normalized = objective.to_ascii_lowercase();
    let intent = if normalized.trim().is_empty() {
        BehavioralTaskIntent::Unknown
    } else if contains_any(
        &normalized,
        &["read-only", "read only", "inspect", "analyze"],
    ) {
        BehavioralTaskIntent::ReadOnlyAnalysis
    } else if contains_any(
        &normalized,
        &["dependency", "upgrade", "migration", "migrate"],
    ) {
        BehavioralTaskIntent::DependencyMigration
    } else if contains_any(&normalized, &["test", "assertion", "coverage", "spec"]) {
        BehavioralTaskIntent::TestRepair
    } else if contains_any(&normalized, &["recover", "resume", "restart"]) {
        BehavioralTaskIntent::RecoveryResume
    } else if contains_any(&normalized, &["document", "readme", "docs"]) {
        BehavioralTaskIntent::Documentation
    } else if contains_any(&normalized, &["browser", "ui", "frontend", "interface"]) {
        BehavioralTaskIntent::UiBrowser
    } else if contains_any(&normalized, &["refactor", "restructure", "modularize"]) {
        BehavioralTaskIntent::Refactor
    } else if contains_any(
        &normalized,
        &["fix", "repair", "bug", "error", "failure", "regression"],
    ) {
        BehavioralTaskIntent::BugFix
    } else if mutation_count > 0 {
        BehavioralTaskIntent::Feature
    } else {
        BehavioralTaskIntent::Other
    };

    let mut language_families = changed_paths
        .iter()
        .filter_map(|path| path.rsplit('.').next())
        .filter_map(language_family)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    language_families.sort();
    language_families.dedup();

    let risk_class = if mutation_count == 0 {
        BehavioralRiskClass::ReadOnly
    } else if tools.iter().any(|tool| {
        let name = tool.tool.to_ascii_lowercase();
        name.contains("shell") || name.contains("browser") || name.contains("network")
    }) {
        BehavioralRiskClass::High
    } else if mutation_count > 3 || changed_paths.len() > 12 {
        BehavioralRiskClass::Medium
    } else {
        BehavioralRiskClass::Low
    };

    let complexity_band = match event_count {
        0..=8 => BehavioralComplexityBand::Small,
        9..=24 => BehavioralComplexityBand::Medium,
        25.. => BehavioralComplexityBand::Large,
    };

    let mut task_features = objective
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|term| term.len() >= 2)
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    task_features.sort();
    task_features.dedup();

    let mut unknowns = Vec::new();
    if objective.trim().is_empty() {
        unknowns.push("task_objective".to_owned());
    }
    if language_families.is_empty() {
        unknowns.push("language_family".to_owned());
    }
    if repository_revision.is_none() {
        unknowns.push("repository_revision".to_owned());
    }

    BehavioralTaskClassificationV1 {
        schema_version: TASK_CLASSIFICATION_SCHEMA_VERSION,
        workspace_mode: repository_revision.map_or(BehavioralWorkspaceMode::Unknown, |_| {
            BehavioralWorkspaceMode::Git
        }),
        intent,
        language_families,
        risk_class,
        complexity_band,
        task_features,
        unknowns,
    }
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn language_family(extension: &str) -> Option<&'static str> {
    match extension.to_ascii_lowercase().as_str() {
        "rs" => Some("rust"),
        "ts" | "tsx" | "js" | "jsx" => Some("typescript-javascript"),
        "py" => Some("python"),
        "go" => Some("go"),
        "java" | "kt" => Some("jvm"),
        "c" | "h" | "cc" | "cpp" | "hpp" => Some("c-cpp"),
        "cs" => Some("csharp"),
        "rb" => Some("ruby"),
        "php" => Some("php"),
        "swift" => Some("swift"),
        "md" | "mdx" => Some("markdown"),
        "toml" | "yaml" | "yml" | "json" => Some("configuration"),
        _ => None,
    }
}

fn delegated_worker_events(events: &[EventEnvelope]) -> bool {
    if events
        .iter()
        .any(|event| matches!(&event.actor, Actor::Worker(_)))
    {
        return true;
    }
    events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::SessionCreated { objective } => Some(objective.trim_start()),
            _ => None,
        })
        .is_some_and(|objective| {
            objective.starts_with("Implement delegated task `")
                || objective
                    .starts_with("Collect read-only repository evidence for the parent goal.")
                || objective.starts_with(
                    "Perform a read-only risk and failure-mode review for the parent goal.",
                )
        })
}

fn model_execution_index(
    request_id: Option<&str>,
    request_indexes: &BTreeMap<String, usize>,
    executions: &[BehavioralModelExecutionV1],
) -> Option<usize> {
    match request_id {
        Some(request_id) => request_indexes.get(request_id).copied(),
        None => executions.len().checked_sub(1),
    }
}

fn find_tool(
    tools: &[BehavioralToolExecutionV1],
    active: Option<usize>,
    tool: &str,
) -> Option<usize> {
    active
        .filter(|index| tools.get(*index).is_some_and(|item| item.tool == tool))
        .or_else(|| {
            tools
                .iter()
                .rposition(|item| item.tool == tool && !item.completed)
        })
}

fn receipt_id(value: &Value) -> Option<String> {
    ["receipt_id", "id", "commit", "integrated_head"]
        .into_iter()
        .find_map(|key| value.get(key).and_then(Value::as_str).map(str::to_owned))
}

fn decision_is_denial(value: &Value) -> bool {
    ["decision", "status", "kind"]
        .into_iter()
        .filter_map(|key| value.get(key).and_then(Value::as_str))
        .any(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "deny" | "denied" | "rejected"
            )
        })
}

fn observed_token_usage(executions: &[BehavioralModelExecutionV1]) -> Option<u64> {
    let mut total = 0u64;
    let mut observed = false;
    for execution in executions {
        let Some(usage) = execution.usage.as_ref() else {
            continue;
        };
        let mut execution_total = None;
        for key in ["total_tokens", "total", "tokens"] {
            if let Some(value) = usage.get(key).and_then(Value::as_u64) {
                execution_total = Some(value);
                break;
            }
        }
        let value = execution_total.or_else(|| {
            let input = ["input_tokens", "prompt_tokens", "input"]
                .into_iter()
                .find_map(|key| usage.get(key).and_then(Value::as_u64));
            let output = ["output_tokens", "completion_tokens", "output"]
                .into_iter()
                .find_map(|key| usage.get(key).and_then(Value::as_u64));
            input
                .zip(output)
                .map(|(input, output)| input.saturating_add(output))
        });
        if let Some(value) = value {
            observed = true;
            total = total.saturating_add(value);
        }
    }
    observed.then_some(total)
}

fn observed_monetary_cost(records: &[Value]) -> Option<u64> {
    let mut total = 0u64;
    let mut observed = false;
    for record in records {
        let value = [
            "monetary_cost_microunits",
            "cost_microunits",
            "cost_microusd",
        ]
        .into_iter()
        .find_map(|key| record.get(key).and_then(Value::as_u64));
        if let Some(value) = value {
            observed = true;
            total = total.saturating_add(value);
        }
    }
    observed.then_some(total)
}

fn event_id(event: &EventEnvelope) -> String {
    event.event_id.to_string()
}

fn event_unix_ms(event: &EventEnvelope) -> i64 {
    let nanos = event.timestamp.unix_timestamp_nanos();
    i64::try_from(nanos / 1_000_000).unwrap_or_else(|_| {
        if nanos.is_negative() {
            i64::MIN
        } else {
            i64::MAX
        }
    })
}

fn normalized_outcome_id(outcome: &BehavioralOutcomeV1) -> Result<String, String> {
    let bytes = serde_json::to_vec(&(
        outcome.schema_version,
        &outcome.session_id,
        &outcome.source_event_ids,
        &outcome.source_event_checksums,
        outcome.root_task_eligible,
        outcome.terminal_status,
        outcome.verified_success,
        &outcome.verification_receipt_ids,
        &outcome.integration_receipt_ids,
    ))
    .map_err(|error| error.to_string())?;
    Ok(format!(
        "behavioral-outcome-{}",
        crate::encode(Sha256::digest(bytes))
    ))
}
