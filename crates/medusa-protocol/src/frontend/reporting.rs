//! Shared user-facing execution reporting derived from the canonical session journal.
//!
//! This module is deliberately presentation-oriented: it exposes concise semantic milestones and
//! authoritative completion/verification state, never model scratchpad or raw tool payloads. Every
//! frontend consumes the same projection so formatting can vary without status drift.

use serde::{Deserialize, Serialize};

use crate::{EventEnvelope, EventPayload, SessionState};

use super::FrontendKind;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionReportKind {
    Inspect,
    Finding,
    PlanChange,
    Implementation,
    Verification,
    Roadblock,
    Recovery,
    Result,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionReportStatus {
    Running,
    Passed,
    Failed,
    Blocked,
    Resolved,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionReportDetail {
    Concise,
    Debug,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationState {
    NotRun,
    Running,
    Passed,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionState {
    InProgress,
    VerifiedSuccess,
    Partial,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionReportEvent {
    pub event_id: String,
    pub kind: ExecutionReportKind,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default)]
    pub scope: Vec<String>,
    pub status: ExecutionReportStatus,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub source_sequence: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionResultSummary {
    pub completion: CompletionState,
    pub verification: VerificationState,
    #[serde(default)]
    pub changed_paths: Vec<String>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_blocker: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionReportSnapshot {
    pub events: Vec<ExecutionReportEvent>,
    pub completion: CompletionState,
    pub verification: VerificationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_blocker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ExecutionResultSummary>,
    pub source_cursor: u64,
}

impl Default for ExecutionReportSnapshot {
    fn default() -> Self {
        Self {
            events: Vec::new(),
            completion: CompletionState::InProgress,
            verification: VerificationState::NotRun,
            active_blocker: None,
            result: None,
            source_cursor: 0,
        }
    }
}

impl ExecutionReportSnapshot {
    /// Stable semantic comparison used by frontend conformance tests. Surface formatting is
    /// intentionally excluded because it is not authoritative.
    #[must_use]
    pub fn semantically_matches(&self, other: &Self) -> bool {
        self.events == other.events
            && self.completion == other.completion
            && self.verification == other.verification
            && self.active_blocker == other.active_blocker
            && self.result == other.result
            && self.source_cursor == other.source_cursor
    }
}

/// Deterministically rebuilds the current user-visible state from canonical journal events.
/// Invalid events are ignored instead of allowing an untrusted payload to become presentation
/// authority. Replaying the same journal yields the same semantic snapshot and never appends UI
/// duplicates.
#[must_use]
pub fn project_execution_report(
    journal: &[EventEnvelope],
    detail: ExecutionReportDetail,
) -> ExecutionReportSnapshot {
    let mut ordered = journal
        .iter()
        .filter(|event| event.validate().is_ok())
        .collect::<Vec<_>>();
    ordered.sort_by_key(|event| event.sequence);

    let mut snapshot = ExecutionReportSnapshot::default();
    for event in ordered {
        if event.sequence <= snapshot.source_cursor {
            continue;
        }
        apply_event(&mut snapshot, event, detail);
        snapshot.source_cursor = event.sequence;
    }
    snapshot
}

/// All shipped frontends intentionally call the same semantic authority. `frontend` is accepted so
/// adapters have a single obvious entry point while remaining unable to alter completion or
/// verification semantics locally.
#[must_use]
pub fn project_execution_report_for_frontend(
    journal: &[EventEnvelope],
    detail: ExecutionReportDetail,
    frontend: FrontendKind,
) -> ExecutionReportSnapshot {
    let _ = frontend;
    project_execution_report(journal, detail)
}

fn apply_event(
    snapshot: &mut ExecutionReportSnapshot,
    event: &EventEnvelope,
    detail_level: ExecutionReportDetail,
) {
    match &event.payload {
        EventPayload::ToolCallRequested { tool, .. }
        | EventPayload::ToolExecutionStarted { tool }
            if is_repository_inspection(tool) =>
        {
            upsert(
                snapshot,
                semantic_event(
                    event,
                    ExecutionReportKind::Inspect,
                    "Repository inspection",
                    None,
                    Vec::new(),
                    ExecutionReportStatus::Running,
                    Vec::new(),
                ),
            );
        }
        EventPayload::ToolExecutionCompleted { tool, exit_code }
            if is_repository_inspection(tool) =>
        {
            upsert(
                snapshot,
                semantic_event(
                    event,
                    ExecutionReportKind::Inspect,
                    "Repository inspection",
                    match detail_level {
                        ExecutionReportDetail::Concise => None,
                        ExecutionReportDetail::Debug => Some(format!(
                            "{tool} exit {}",
                            exit_code.map_or_else(|| "0".to_owned(), |code| code.to_string())
                        )),
                    },
                    Vec::new(),
                    if exit_code.is_none_or(|code| code == 0) {
                        ExecutionReportStatus::Passed
                    } else {
                        ExecutionReportStatus::Failed
                    },
                    Vec::new(),
                ),
            );
        }
        EventPayload::WorkerEvidenceRecorded { evidence } => {
            let evidence_ref = value_string(
                evidence,
                &["evidence_ref", "evidenceRef", "artifact_ref", "artifactRef"],
            );
            upsert(
                snapshot,
                semantic_event(
                    event,
                    ExecutionReportKind::Finding,
                    "Evidence-backed finding",
                    safe_value_summary(evidence),
                    Vec::new(),
                    ExecutionReportStatus::Passed,
                    evidence_ref.into_iter().collect(),
                ),
            );
        }
        EventPayload::PlanUpdated { .. } => {
            upsert(
                snapshot,
                semantic_event(
                    event,
                    ExecutionReportKind::PlanChange,
                    "Implementation plan changed",
                    None,
                    Vec::new(),
                    ExecutionReportStatus::Resolved,
                    Vec::new(),
                ),
            );
        }
        EventPayload::FileTransactionCommitted {
            paths,
            rollback_ref,
        } => {
            upsert(
                snapshot,
                semantic_event(
                    event,
                    ExecutionReportKind::Implementation,
                    "Implemented repository changes",
                    None,
                    paths.iter().map(|path| redact_text(path)).collect(),
                    ExecutionReportStatus::Passed,
                    vec![redact_text(rollback_ref)],
                ),
            );
        }
        EventPayload::IntegrationReceiptRecorded { receipt } => {
            upsert(
                snapshot,
                semantic_event(
                    event,
                    ExecutionReportKind::Implementation,
                    "Integrated implementation changes",
                    safe_value_summary(receipt),
                    Vec::new(),
                    ExecutionReportStatus::Passed,
                    value_string(
                        receipt,
                        &["evidence_ref", "evidenceRef", "artifact_ref", "artifactRef"],
                    )
                    .into_iter()
                    .collect(),
                ),
            );
        }
        EventPayload::VerificationStarted { commands } => {
            snapshot.verification = VerificationState::Running;
            let detail = match detail_level {
                ExecutionReportDetail::Concise => None,
                ExecutionReportDetail::Debug => Some(
                    commands
                        .iter()
                        .map(|command| redact_text(command))
                        .collect::<Vec<_>>()
                        .join("; "),
                ),
            };
            upsert(
                snapshot,
                semantic_event(
                    event,
                    ExecutionReportKind::Verification,
                    "Verification",
                    detail,
                    Vec::new(),
                    ExecutionReportStatus::Running,
                    Vec::new(),
                ),
            );
        }
        EventPayload::VerificationCompleted { passed, evidence } => {
            snapshot.verification = if *passed {
                VerificationState::Passed
            } else {
                VerificationState::Failed
            };
            upsert(
                snapshot,
                semantic_event(
                    event,
                    ExecutionReportKind::Verification,
                    "Verification",
                    Some(if *passed {
                        "Required checks passed".to_owned()
                    } else {
                        "Required checks reported a failure".to_owned()
                    }),
                    Vec::new(),
                    if *passed {
                        ExecutionReportStatus::Passed
                    } else {
                        ExecutionReportStatus::Failed
                    },
                    evidence.iter().map(|value| redact_text(value)).collect(),
                ),
            );
        }
        EventPayload::ToolCallDenied { tool, reason } => {
            let message = format!("{tool}: {}", redact_text(reason));
            snapshot.active_blocker = Some(message.clone());
            upsert(
                snapshot,
                semantic_event(
                    event,
                    ExecutionReportKind::Roadblock,
                    "Execution blocked",
                    Some(message),
                    Vec::new(),
                    ExecutionReportStatus::Blocked,
                    Vec::new(),
                ),
            );
        }
        EventPayload::SessionPaused { reason } => {
            let reason = redact_text(reason);
            snapshot.active_blocker = Some(reason.clone());
            upsert(
                snapshot,
                semantic_event(
                    event,
                    ExecutionReportKind::Roadblock,
                    "Execution blocked",
                    Some(reason),
                    Vec::new(),
                    ExecutionReportStatus::Blocked,
                    Vec::new(),
                ),
            );
        }
        EventPayload::SessionStateChanged { to, .. } if *to == SessionState::Blocked => {
            snapshot.active_blocker = Some("Session entered blocked state".to_owned());
            upsert(
                snapshot,
                semantic_event(
                    event,
                    ExecutionReportKind::Roadblock,
                    "Execution blocked",
                    snapshot.active_blocker.clone(),
                    Vec::new(),
                    ExecutionReportStatus::Blocked,
                    Vec::new(),
                ),
            );
        }
        EventPayload::CheckpointRestoreRequested { checkpoint_id, .. } => {
            snapshot.active_blocker = None;
            upsert(
                snapshot,
                semantic_event(
                    event,
                    ExecutionReportKind::Recovery,
                    "Recovery path",
                    match detail_level {
                        ExecutionReportDetail::Concise => None,
                        ExecutionReportDetail::Debug => {
                            Some(format!("Restoring checkpoint {}", redact_text(checkpoint_id)))
                        }
                    },
                    Vec::new(),
                    ExecutionReportStatus::Running,
                    Vec::new(),
                ),
            );
        }
        EventPayload::RecoveryActionCompleted { receipt } => {
            snapshot.active_blocker = None;
            upsert(
                snapshot,
                semantic_event(
                    event,
                    ExecutionReportKind::Recovery,
                    "Recovery path",
                    safe_value_summary(receipt),
                    Vec::new(),
                    ExecutionReportStatus::Resolved,
                    value_string(
                        receipt,
                        &["evidence_ref", "evidenceRef", "artifact_ref", "artifactRef"],
                    )
                    .into_iter()
                    .collect(),
                ),
            );
        }
        EventPayload::RuntimeFailed { message } => {
            let message = redact_text(message);
            snapshot.active_blocker = Some(message.clone());
            upsert(
                snapshot,
                semantic_event(
                    event,
                    ExecutionReportKind::Roadblock,
                    "Runtime failure",
                    Some(message),
                    Vec::new(),
                    ExecutionReportStatus::Failed,
                    Vec::new(),
                ),
            );
        }
        EventPayload::SessionCompleted { report_ref } => {
            snapshot.completion = if snapshot.verification == VerificationState::Passed {
                CompletionState::VerifiedSuccess
            } else {
                CompletionState::Partial
            };
            let evidence_refs = (!report_ref.trim().is_empty())
                .then(|| redact_text(report_ref))
                .into_iter()
                .collect::<Vec<_>>();
            upsert(
                snapshot,
                semantic_event(
                    event,
                    ExecutionReportKind::Result,
                    if snapshot.completion == CompletionState::VerifiedSuccess {
                        "Task completed and verified"
                    } else {
                        "Task completed with unresolved verification"
                    },
                    None,
                    Vec::new(),
                    if snapshot.completion == CompletionState::VerifiedSuccess {
                        ExecutionReportStatus::Passed
                    } else {
                        ExecutionReportStatus::Blocked
                    },
                    evidence_refs.clone(),
                ),
            );
            snapshot.result = Some(result_summary(snapshot, evidence_refs));
        }
        EventPayload::SessionFailed { error } => {
            snapshot.completion = CompletionState::Failed;
            let message = redact_text(&error.message);
            snapshot.active_blocker = Some(message.clone());
            upsert(
                snapshot,
                semantic_event(
                    event,
                    ExecutionReportKind::Result,
                    "Task failed",
                    Some(message),
                    Vec::new(),
                    ExecutionReportStatus::Failed,
                    Vec::new(),
                ),
            );
            snapshot.result = Some(result_summary(snapshot, Vec::new()));
        }
        EventPayload::CancellationCompleted => {
            snapshot.completion = CompletionState::Cancelled;
            upsert(
                snapshot,
                semantic_event(
                    event,
                    ExecutionReportKind::Result,
                    "Task cancelled",
                    None,
                    Vec::new(),
                    ExecutionReportStatus::Resolved,
                    Vec::new(),
                ),
            );
            snapshot.result = Some(result_summary(snapshot, Vec::new()));
        }
        _ => {}
    }
}

fn semantic_event(
    source: &EventEnvelope,
    kind: ExecutionReportKind,
    label: impl Into<String>,
    detail: Option<String>,
    scope: Vec<String>,
    status: ExecutionReportStatus,
    evidence_refs: Vec<String>,
) -> ExecutionReportEvent {
    ExecutionReportEvent {
        event_id: format!("report:{}", source.event_id),
        kind,
        label: label.into(),
        detail: detail.map(|value| redact_text(&value)),
        scope,
        status,
        evidence_refs,
        source_sequence: source.sequence,
    }
}

fn upsert(snapshot: &mut ExecutionReportSnapshot, event: ExecutionReportEvent) {
    if let Some(existing) = snapshot
        .events
        .iter_mut()
        .find(|existing| existing.kind == event.kind && existing.label == event.label)
    {
        existing.status = event.status;
        existing.source_sequence = event.source_sequence;
        existing.event_id = event.event_id;
        if event.detail.is_some() {
            existing.detail = event.detail;
        }
        merge_unique(&mut existing.scope, event.scope);
        merge_unique(&mut existing.evidence_refs, event.evidence_refs);
        return;
    }
    snapshot.events.push(event);
}

fn merge_unique(target: &mut Vec<String>, incoming: Vec<String>) {
    for value in incoming {
        if !target.contains(&value) {
            target.push(value);
        }
    }
}

fn result_summary(snapshot: &ExecutionReportSnapshot, evidence_refs: Vec<String>) -> ExecutionResultSummary {
    let changed_paths = snapshot
        .events
        .iter()
        .filter(|event| event.kind == ExecutionReportKind::Implementation)
        .flat_map(|event| event.scope.iter().cloned())
        .fold(Vec::new(), |mut paths, path| {
            if !paths.contains(&path) {
                paths.push(path);
            }
            paths
        });
    ExecutionResultSummary {
        completion: snapshot.completion,
        verification: snapshot.verification,
        changed_paths,
        evidence_refs,
        remaining_blocker: snapshot.active_blocker.clone(),
    }
}

fn is_repository_inspection(tool: &str) -> bool {
    let normalized = tool.to_ascii_lowercase();
    normalized.contains("read")
        || normalized.contains("search")
        || normalized.contains("find")
        || normalized.contains("list")
        || normalized.contains("fetch_file")
}

fn value_string(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(serde_json::Value::as_str))
        .filter(|value| !value.trim().is_empty())
        .map(redact_text)
}

fn safe_value_summary(value: &serde_json::Value) -> Option<String> {
    value_string(value, &["summary", "message", "result", "status"])
}

fn secret_like(key: &str) -> bool {
    let normalized = key
        .trim_matches(|character: char| character == '-' || character == '_' || character == '/')
        .to_ascii_lowercase();
    [
        "token",
        "secret",
        "password",
        "credential",
        "authorization",
        "api_key",
        "apikey",
    ]
    .iter()
    .any(|candidate| normalized.contains(candidate))
}

fn redact_text(value: &str) -> String {
    let mut redact_next = false;
    value
        .split_whitespace()
        .map(|part| {
            if redact_next {
                redact_next = false;
                return "[redacted]".to_owned();
            }
            let lower = part.to_ascii_lowercase();
            if lower == "bearer" {
                redact_next = true;
                return "Bearer".to_owned();
            }
            if let Some((key, _)) = part.split_once('=') {
                if secret_like(key) {
                    return format!("{key}=[redacted]");
                }
            }
            if secret_like(part) && part.starts_with("--") {
                redact_next = true;
                return part.to_owned();
            }
            part.to_owned()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use medusa_core::{CorrelationId, SessionId};
    use serde_json::json;
    use time::OffsetDateTime;

    use crate::{Actor, EventPayload};

    use super::*;

    fn journal(payloads: Vec<EventPayload>) -> Vec<EventEnvelope> {
        let session = SessionId::new();
        let correlation = CorrelationId::new();
        payloads
            .into_iter()
            .enumerate()
            .map(|(index, payload)| {
                EventEnvelope::new(
                    (index + 1) as u64,
                    session.clone(),
                    Actor::Coordinator,
                    correlation.clone(),
                    payload,
                    None,
                    OffsetDateTime::UNIX_EPOCH,
                )
                .expect("event")
            })
            .collect()
    }

    #[test]
    fn repeated_low_level_reads_collapse_to_one_inspection_step() {
        let mut payloads = Vec::new();
        for _ in 0..10 {
            payloads.push(EventPayload::ToolExecutionStarted {
                tool: "fetch_file".to_owned(),
            });
            payloads.push(EventPayload::ToolExecutionCompleted {
                tool: "fetch_file".to_owned(),
                exit_code: Some(0),
            });
        }
        let report = project_execution_report(&journal(payloads), ExecutionReportDetail::Concise);
        let inspections = report
            .events
            .iter()
            .filter(|event| event.kind == ExecutionReportKind::Inspect)
            .collect::<Vec<_>>();
        assert_eq!(inspections.len(), 1);
        assert_eq!(inspections[0].status, ExecutionReportStatus::Passed);
    }

    #[test]
    fn finding_is_deduplicated_and_secret_data_is_redacted() {
        let report = project_execution_report(
            &journal(vec![
                EventPayload::WorkerEvidenceRecorded {
                    evidence: json!({"summary": "Architecture finding", "evidence_ref": "receipt-1"}),
                },
                EventPayload::WorkerEvidenceRecorded {
                    evidence: json!({"summary": "Architecture finding", "evidence_ref": "receipt-1"}),
                },
                EventPayload::VerificationStarted {
                    commands: vec!["cargo test --token=supersecret --api_key abc123".to_owned()],
                },
            ]),
            ExecutionReportDetail::Debug,
        );
        assert_eq!(
            report
                .events
                .iter()
                .filter(|event| event.kind == ExecutionReportKind::Finding)
                .count(),
            1
        );
        let encoded = serde_json::to_string(&report).expect("serialize");
        assert!(!encoded.contains("supersecret"));
        assert!(!encoded.contains("abc123"));
        assert!(encoded.contains("[redacted]"));
    }

    #[test]
    fn blocker_recovery_and_verified_result_are_authoritative() {
        let report = project_execution_report(
            &journal(vec![
                EventPayload::SessionPaused {
                    reason: "Windows-only validation unavailable locally".to_owned(),
                },
                EventPayload::RecoveryActionCompleted {
                    receipt: json!({"summary": "Selected authoritative CI validation"}),
                },
                EventPayload::FileTransactionCommitted {
                    paths: vec!["src/lib.rs".to_owned()],
                    rollback_ref: "rollback-1".to_owned(),
                },
                EventPayload::VerificationStarted {
                    commands: vec!["cargo test".to_owned(), "cargo clippy".to_owned()],
                },
                EventPayload::VerificationCompleted {
                    passed: true,
                    evidence: vec!["ci-run-1".to_owned()],
                },
                EventPayload::SessionCompleted {
                    report_ref: "report-1".to_owned(),
                },
            ]),
            ExecutionReportDetail::Concise,
        );
        let kinds = report.events.iter().map(|event| event.kind).collect::<Vec<_>>();
        assert!(kinds.windows(2).any(|pair| pair == [ExecutionReportKind::Roadblock, ExecutionReportKind::Recovery]));
        assert_eq!(report.active_blocker, None);
        assert_eq!(report.verification, VerificationState::Passed);
        assert_eq!(report.completion, CompletionState::VerifiedSuccess);
        assert_eq!(report.result.as_ref().expect("result").changed_paths, vec!["src/lib.rs"]);
    }

    #[test]
    fn completion_without_passing_verification_is_partial_not_success() {
        let report = project_execution_report(
            &journal(vec![
                EventPayload::VerificationCompleted {
                    passed: false,
                    evidence: vec!["failing-check".to_owned()],
                },
                EventPayload::SessionCompleted {
                    report_ref: "report-1".to_owned(),
                },
            ]),
            ExecutionReportDetail::Concise,
        );
        assert_eq!(report.verification, VerificationState::Failed);
        assert_eq!(report.completion, CompletionState::Partial);
        assert_eq!(
            report.events.last().expect("result").status,
            ExecutionReportStatus::Blocked
        );
    }

    #[test]
    fn concise_and_debug_change_detail_not_semantic_authority() {
        let events = journal(vec![
            EventPayload::VerificationStarted {
                commands: vec!["cargo test --workspace".to_owned()],
            },
            EventPayload::VerificationCompleted {
                passed: true,
                evidence: vec!["receipt-1".to_owned()],
            },
        ]);
        let concise = project_execution_report(&events, ExecutionReportDetail::Concise);
        let debug = project_execution_report(&events, ExecutionReportDetail::Debug);
        assert_eq!(concise.completion, debug.completion);
        assert_eq!(concise.verification, debug.verification);
        assert_eq!(concise.events.len(), debug.events.len());
        assert_eq!(concise.events[0].kind, debug.events[0].kind);
        assert_eq!(concise.events[0].status, debug.events[0].status);
    }

    #[test]
    fn replay_and_all_frontends_have_identical_semantic_state() {
        let events = journal(vec![
            EventPayload::ToolExecutionStarted {
                tool: "search_repository".to_owned(),
            },
            EventPayload::ToolExecutionCompleted {
                tool: "search_repository".to_owned(),
                exit_code: Some(0),
            },
            EventPayload::FileTransactionCommitted {
                paths: vec!["src/main.rs".to_owned()],
                rollback_ref: "rollback-1".to_owned(),
            },
            EventPayload::VerificationCompleted {
                passed: true,
                evidence: vec!["tests-pass".to_owned()],
            },
            EventPayload::SessionCompleted {
                report_ref: "report-1".to_owned(),
            },
        ]);
        let replayed = project_execution_report(&events, ExecutionReportDetail::Concise);
        assert!(replayed.semantically_matches(&project_execution_report(
            &events,
            ExecutionReportDetail::Concise
        )));
        for frontend in [
            FrontendKind::Headless,
            FrontendKind::Tui,
            FrontendKind::Desktop,
            FrontendKind::Telegram,
            FrontendKind::Other,
        ] {
            let surface = project_execution_report_for_frontend(
                &events,
                ExecutionReportDetail::Concise,
                frontend,
            );
            assert!(replayed.semantically_matches(&surface));
        }
    }

    #[test]
    fn raw_assistant_planning_and_tool_arguments_never_enter_reporting_payloads() {
        let report = project_execution_report(
            &journal(vec![
                EventPayload::AssistantMessageRecorded {
                    message: json!({"role":"assistant", "content":"hidden scratchpad phrase"}),
                },
                EventPayload::ToolCallRequested {
                    tool: "shell".to_owned(),
                    arguments: json!({"password":"do-not-leak", "command":"internal retry"}),
                },
            ]),
            ExecutionReportDetail::Debug,
        );
        let encoded = serde_json::to_string(&report).expect("serialize");
        assert!(!encoded.contains("hidden scratchpad phrase"));
        assert!(!encoded.contains("do-not-leak"));
        assert!(!encoded.contains("internal retry"));
    }
}
