//! Read-only, versioned audit reports derived from durable session state.

use std::path::Path;

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_protocol::{Actor, EventPayload};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{ApprovalReceipt, RollbackReceipt, session};

pub const AUDIT_REPORT_SCHEMA_VERSION: &str = "medusa.session-audit/v1";
const MAX_STRING_BYTES: usize = 4_096;
const MAX_ARRAY_ITEMS: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditReportFormat {
    Markdown,
    Json,
}

impl AuditReportFormat {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "markdown" | "md" => Ok(Self::Markdown),
            "json" => Ok(Self::Json),
            other => Err(format!("unsupported report format `{other}`; use markdown or json")),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SessionAuditReport {
    pub schema_version: String,
    pub session_id: String,
    pub objective: String,
    pub repository: String,
    pub created_at: String,
    pub updated_at: String,
    pub status: String,
    pub turn_count: u32,
    pub plan: Vec<Value>,
    pub provider_routes: Vec<ProviderRoute>,
    pub timeline: Vec<AuditEvent>,
    pub files_changed: Vec<String>,
    pub commands_requested: Vec<Value>,
    pub commands_executed: Vec<CommandExecution>,
    pub approvals: Vec<Value>,
    pub rollbacks: Vec<Value>,
    pub verification: Vec<Value>,
    pub durable_evidence: Vec<String>,
    pub artifact_references: Vec<String>,
    pub completion_reason: String,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProviderRoute {
    pub provider: String,
    pub model: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AuditEvent {
    pub sequence: u64,
    pub timestamp: String,
    pub actor: String,
    pub event_type: String,
    pub data: Value,
    pub checksum: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CommandExecution {
    pub tool: String,
    pub exit_code: Option<i32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Provenance {
    pub first_event_checksum: Option<String>,
    pub final_event_checksum: Option<String>,
    pub report_fingerprint: String,
}

pub fn generate_session_audit_report(repo: &Path, session_id: &str) -> MedusaResult<SessionAuditReport> {
    let durable = session::load(repo, session_id)?;
    let mut provider_routes = Vec::new();
    let mut timeline = Vec::new();
    let mut files_changed = Vec::new();
    let mut commands_requested = Vec::new();
    let mut commands_executed = Vec::new();
    let mut verification = Vec::new();
    let mut completion_reason = if durable.completed {
        "completed".to_owned()
    } else {
        "interrupted_or_in_progress".to_owned()
    };

    for envelope in &durable.events {
        match &envelope.payload {
            EventPayload::ModelRequestStarted { provider, model } => {
                let route = ProviderRoute {
                    provider: redact_string(provider),
                    model: redact_string(model),
                };
                if !provider_routes.iter().any(|item: &ProviderRoute| {
                    item.provider == route.provider && item.model == route.model
                }) {
                    provider_routes.push(route);
                }
            }
            EventPayload::ToolCallRequested { tool, arguments } => {
                commands_requested.push(serde_json::json!({
                    "tool": tool,
                    "arguments": sanitize_value(arguments),
                }));
            }
            EventPayload::ToolExecutionCompleted { tool, exit_code } => {
                commands_executed.push(CommandExecution {
                    tool: tool.clone(),
                    exit_code: *exit_code,
                });
            }
            EventPayload::FileTransactionCommitted { paths, .. } => {
                for path in paths {
                    if !files_changed.contains(path) {
                        files_changed.push(path.clone());
                    }
                }
            }
            EventPayload::VerificationStarted { commands } => {
                verification.push(serde_json::json!({"started": sanitize_value(&serde_json::json!(commands))}));
            }
            EventPayload::VerificationCompleted { passed, evidence } => {
                verification.push(serde_json::json!({
                    "passed": passed,
                    "evidence": sanitize_value(&serde_json::json!(evidence)),
                }));
            }
            EventPayload::SessionCompleted { report_ref } => {
                completion_reason = format!("verified_completion:{report_ref}");
            }
            EventPayload::SessionFailed { error } => {
                completion_reason = format!("failed:{}", redact_string(&error.to_string()));
            }
            EventPayload::SessionPaused { reason } => {
                completion_reason = format!("paused:{}", redact_string(reason));
            }
            _ => {}
        }
        let encoded = serde_json::to_value(&envelope.payload).map_err(serialization_error)?;
        let event_type = encoded
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        let data = encoded.get("data").cloned().unwrap_or(Value::Null);
        timeline.push(AuditEvent {
            sequence: envelope.sequence,
            timestamp: envelope.timestamp.to_string(),
            actor: actor_name(&envelope.actor),
            event_type,
            data: sanitize_value(&data),
            checksum: envelope.checksum.clone(),
        });
    }

    let plan = durable
        .plan
        .iter()
        .map(|step| serde_json::to_value(step).map(|value| sanitize_value(&value)))
        .collect::<Result<Vec<_>, _>>()
        .map_err(serialization_error)?;
    let approvals = sanitize_serializable(&durable.approval_receipts)?;
    let rollbacks = sanitize_serializable(&durable.rollback_receipts)?;
    let durable_evidence = durable.evidence.iter().map(|item| redact_string(item)).collect();
    let artifact_references = durable
        .tool_artifacts
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    let first_event_checksum = durable.events.first().map(|event| event.checksum.clone());
    let final_event_checksum = durable.events.last().map(|event| event.checksum.clone());

    let mut report = SessionAuditReport {
        schema_version: AUDIT_REPORT_SCHEMA_VERSION.to_owned(),
        session_id: durable.id.to_string(),
        objective: redact_string(&durable.objective),
        repository: durable.repo.display().to_string(),
        created_at: durable.created_at.to_string(),
        updated_at: durable.updated_at.to_string(),
        status: if durable.completed { "completed" } else { "incomplete" }.to_owned(),
        turn_count: durable.turn,
        plan,
        provider_routes,
        timeline,
        files_changed,
        commands_requested,
        commands_executed,
        approvals,
        rollbacks,
        verification,
        durable_evidence,
        artifact_references,
        completion_reason,
        provenance: Provenance {
            first_event_checksum,
            final_event_checksum,
            report_fingerprint: String::new(),
        },
    };
    report.provenance.report_fingerprint = report_fingerprint(&report)?;
    Ok(report)
}

pub fn render_session_audit_report(report: &SessionAuditReport, format: AuditReportFormat) -> MedusaResult<String> {
    match format {
        AuditReportFormat::Json => serde_json::to_string_pretty(report).map_err(serialization_error),
        AuditReportFormat::Markdown => Ok(render_markdown(report)),
    }
}

fn render_markdown(report: &SessionAuditReport) -> String {
    let mut output = format!(
        "# Medusa Session Audit Report\n\n- Schema: `{}`\n- Session: `{}`\n- Status: `{}`\n- Created: `{}`\n- Updated: `{}`\n- Repository: `{}`\n- Completion reason: `{}`\n- Report fingerprint: `{}`\n\n## Objective\n\n{}\n\n",
        report.schema_version,
        report.session_id,
        report.status,
        report.created_at,
        report.updated_at,
        report.repository,
        report.completion_reason,
        report.provenance.report_fingerprint,
        report.objective,
    );
    output.push_str("## Plan\n\n");
    for step in &report.plan {
        output.push_str(&format!("- `{}`\n", compact_json(step)));
    }
    output.push_str("\n## Provider routes\n\n");
    for route in &report.provider_routes {
        output.push_str(&format!("- `{}` / `{}`\n", route.provider, route.model));
    }
    output.push_str("\n## Files changed\n\n");
    for path in &report.files_changed {
        output.push_str(&format!("- `{path}`\n"));
    }
    output.push_str("\n## Verification\n\n");
    for item in &report.verification {
        output.push_str(&format!("- `{}`\n", compact_json(item)));
    }
    output.push_str("\n## Approvals and denials\n\n");
    for item in &report.approvals {
        output.push_str(&format!("- `{}`\n", compact_json(item)));
    }
    output.push_str("\n## Rollbacks\n\n");
    for item in &report.rollbacks {
        output.push_str(&format!("- `{}`\n", compact_json(item)));
    }
    output.push_str("\n## Durable evidence\n\n");
    for item in &report.durable_evidence {
        output.push_str(&format!("- {}\n", item.replace('\n', " ")));
    }
    output.push_str("\n## Event timeline\n\n| Seq | Timestamp | Actor | Event | Checksum |\n|---:|---|---|---|---|\n");
    for event in &report.timeline {
        output.push_str(&format!(
            "| {} | {} | {} | {} | `{}` |\n",
            event.sequence, event.timestamp, event.actor, event.event_type, event.checksum
        ));
    }
    output
}

fn sanitize_serializable<T: Serialize>(items: &[T]) -> MedusaResult<Vec<Value>> {
    items
        .iter()
        .map(|item| serde_json::to_value(item).map(|value| sanitize_value(&value)).map_err(serialization_error))
        .collect()
}

fn sanitize_value(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(redact_string(text)),
        Value::Array(values) => Value::Array(
            values
                .iter()
                .take(MAX_ARRAY_ITEMS)
                .map(sanitize_value)
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    let secret_key = is_secret_like(key);
                    (
                        key.clone(),
                        if secret_key {
                            Value::String("[REDACTED]".to_owned())
                        } else {
                            sanitize_value(value)
                        },
                    )
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

fn redact_string(text: &str) -> String {
    let bounded = if text.len() > MAX_STRING_BYTES {
        format!("{}…[truncated {} bytes]", &text[..MAX_STRING_BYTES], text.len() - MAX_STRING_BYTES)
    } else {
        text.to_owned()
    };
    bounded
        .split_whitespace()
        .map(|token| {
            if is_secret_like(token) || token.starts_with("sk-") || token.starts_with("ghp_") {
                "[REDACTED]"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_secret_like(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("api_key")
        || lower.contains("apikey")
        || lower.contains("authorization")
        || lower.contains("password")
        || lower.contains("secret")
        || lower.contains("token=")
}

fn actor_name(actor: &Actor) -> String {
    match actor {
        Actor::User => "user".to_owned(),
        Actor::Coordinator => "coordinator".to_owned(),
        Actor::Worker(id) => format!("worker:{id}"),
        Actor::System(id) => format!("system:{id}"),
    }
}

fn report_fingerprint(report: &SessionAuditReport) -> MedusaResult<String> {
    let mut canonical = report.clone();
    canonical.provenance.report_fingerprint.clear();
    let bytes = serde_json::to_vec(&canonical).map_err(serialization_error)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned())
}

fn serialization_error(error: serde_json::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidEvent,
        ErrorCategory::Persistence,
        format!("failed to serialize audit report: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use medusa_core::{CorrelationId, SessionId};
    use medusa_protocol::{EventEnvelope, CURRENT_PROTOCOL_VERSION};
    use time::OffsetDateTime;

    use super::*;
    use crate::{AgentPlanStep, AgentPlanStepStatus, AgentSession};

    #[test]
    fn report_is_restart_safe_versioned_and_redacted() {
        let directory = tempfile::tempdir().unwrap();
        let id = SessionId::new();
        let event = EventEnvelope::new(
            1,
            id.clone(),
            Actor::Coordinator,
            CorrelationId::new(),
            EventPayload::ToolCallRequested {
                tool: "shell".to_owned(),
                arguments: serde_json::json!({"command": "echo ok", "api_key": "sk-secret"}),
            },
            None,
            OffsetDateTime::UNIX_EPOCH,
        )
        .unwrap();
        assert_eq!(event.protocol_version, CURRENT_PROTOCOL_VERSION);
        let durable = AgentSession {
            id: id.clone(),
            objective: "inspect api_key=supersecret".to_owned(),
            repo: directory.path().to_path_buf(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            completed: false,
            turn: 1,
            plan: vec![AgentPlanStep { title: "Inspect".to_owned(), status: AgentPlanStepStatus::Completed }],
            pending_question: None,
            messages: Vec::new(),
            events: vec![event],
            evidence: vec!["token=secret output".to_owned()],
            tool_artifacts: vec![directory.path().join("artifact.log")],
            world_model: None,
            approval_grants: Vec::new(),
            approval_receipts: Vec::<ApprovalReceipt>::new(),
            rollback_receipts: Vec::<RollbackReceipt>::new(),
        };
        fs::create_dir_all(directory.path().join(".medusa/sessions")).unwrap();
        fs::write(
            directory.path().join(".medusa/sessions").join(format!("{id}.json")),
            serde_json::to_vec_pretty(&durable).unwrap(),
        )
        .unwrap();

        let report = generate_session_audit_report(directory.path(), id.as_str()).unwrap();
        let json = render_session_audit_report(&report, AuditReportFormat::Json).unwrap();
        assert!(json.contains(AUDIT_REPORT_SCHEMA_VERSION));
        assert!(json.contains("[REDACTED]"));
        assert!(!json.contains("supersecret"));
        assert!(!json.contains("sk-secret"));
        assert!(!report.provenance.report_fingerprint.is_empty());
        let markdown = render_session_audit_report(&report, AuditReportFormat::Markdown).unwrap();
        assert!(markdown.contains("# Medusa Session Audit Report"));
    }
}
