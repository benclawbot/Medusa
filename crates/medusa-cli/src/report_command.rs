use std::{fs, path::{Path, PathBuf}};

use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

const SCHEMA: &str = "medusa.session-audit/v1";
const MAX_STRING: usize = 4096;
const MAX_ITEMS: usize = 128;

#[derive(Serialize)]
struct AuditReport {
    schema_version: &'static str,
    session_id: String,
    objective: Value,
    repository: Value,
    created_at: Value,
    updated_at: Value,
    status: String,
    turn_count: Value,
    plan: Value,
    provider_routes: Vec<Value>,
    orchestration: Vec<Value>,
    files_changed: Vec<String>,
    commands_requested: Vec<Value>,
    commands_executed: Vec<Value>,
    approvals_and_denials: Value,
    containment_decisions: Vec<Value>,
    checkpoints: Vec<Value>,
    failures_retries_and_replans: Vec<Value>,
    rollbacks: Value,
    verification: Vec<Value>,
    artifact_references: Value,
    completion_reason: String,
    event_timeline: Vec<Value>,
    provenance: Provenance,
}

#[derive(Serialize)]
struct Provenance {
    first_event_checksum: Option<String>,
    final_event_checksum: Option<String>,
    report_fingerprint: String,
}

pub fn try_run(repo: &Path, args: &[String]) -> Option<Result<(), String>> {
    let position = args.iter().position(|arg| arg == "report")?;
    let session_id = args.get(position + 1).cloned().ok_or_else(|| {
        "usage: medusa report <session-id> [--format markdown|json] [--output PATH]".to_owned()
    });
    Some(session_id.and_then(|session_id| run(repo, &session_id, args)))
}

fn run(repo: &Path, session_id: &str, args: &[String]) -> Result<(), String> {
    let format = option_value(args, "--format").unwrap_or_else(|| "markdown".to_owned());
    if !matches!(format.as_str(), "markdown" | "md" | "json") {
        return Err("--format must be markdown or json".to_owned());
    }
    let path = session_path(repo, session_id);
    let bytes = fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let session: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    let report = build_report(&session, session_id)?;
    let rendered = if format == "json" {
        serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
    } else {
        markdown(&report)
    };
    if let Some(output) = option_value(args, "--output") {
        fs::write(&output, rendered).map_err(|error| format!("write {output}: {error}"))?;
    } else {
        println!("{rendered}");
    }
    Ok(())
}

fn build_report(session: &Value, requested_id: &str) -> Result<AuditReport, String> {
    let object = session.as_object().ok_or_else(|| "session root must be an object".to_owned())?;
    let events = object.get("events").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut routes = Vec::new();
    let mut orchestration = Vec::new();
    let mut files_changed = Vec::new();
    let mut requested = Vec::new();
    let mut executed = Vec::new();
    let mut containment = Vec::new();
    let mut checkpoints = Vec::new();
    let mut failures = Vec::new();
    let mut verification = Vec::new();
    let mut timeline = Vec::new();
    let mut completion_reason = if object.get("completed").and_then(Value::as_bool) == Some(true) {
        "completed".to_owned()
    } else {
        "interrupted_or_in_progress".to_owned()
    };

    for event in &events {
        let payload = event.get("payload").cloned().unwrap_or(Value::Null);
        let event_type = payload.get("type").and_then(Value::as_str).unwrap_or("unknown");
        let data = payload.get("data").cloned().unwrap_or(Value::Null);
        match event_type {
            "model_request_started" => routes.push(sanitize(&data)),
            "plan_created" | "plan_updated" => orchestration.push(sanitize(&payload)),
            "tool_call_requested" => requested.push(sanitize(&data)),
            "tool_call_denied" => containment.push(sanitize(&data)),
            "tool_execution_completed" => executed.push(sanitize(&data)),
            "file_transaction_committed" => {
                if let Some(paths) = data.get("paths").and_then(Value::as_array) {
                    for path in paths.iter().filter_map(Value::as_str) {
                        if !files_changed.iter().any(|existing| existing == path) {
                            files_changed.push(path.to_owned());
                        }
                    }
                }
            }
            "checkpoint_created" => checkpoints.push(sanitize(&data)),
            "verification_started" | "verification_completed" => verification.push(sanitize(&payload)),
            "session_failed" | "session_paused" | "session_resumed" | "session_state_changed" => failures.push(sanitize(&payload)),
            "session_completed" => completion_reason = "verified_completion".to_owned(),
            _ => {}
        }
        timeline.push(sanitize(event));
    }
    let first = events.first().and_then(|event| event.get("checksum")).and_then(Value::as_str).map(str::to_owned);
    let final_checksum = events.last().and_then(|event| event.get("checksum")).and_then(Value::as_str).map(str::to_owned);
    let mut report = AuditReport {
        schema_version: SCHEMA,
        session_id: object.get("id").and_then(Value::as_str).unwrap_or(requested_id).to_owned(),
        objective: sanitize(object.get("objective").unwrap_or(&Value::Null)),
        repository: sanitize(object.get("repo").unwrap_or(&Value::Null)),
        created_at: sanitize(object.get("created_at").unwrap_or(&Value::Null)),
        updated_at: sanitize(object.get("updated_at").unwrap_or(&Value::Null)),
        status: if object.get("completed").and_then(Value::as_bool) == Some(true) { "completed" } else { "incomplete" }.to_owned(),
        turn_count: object.get("turn").cloned().unwrap_or(Value::Null),
        plan: sanitize(object.get("plan").unwrap_or(&Value::Null)),
        provider_routes: routes,
        orchestration,
        files_changed,
        commands_requested: requested,
        commands_executed: executed,
        approvals_and_denials: sanitize(object.get("approval_receipts").unwrap_or(&Value::Null)),
        containment_decisions: containment,
        checkpoints,
        failures_retries_and_replans: failures,
        rollbacks: sanitize(object.get("rollback_receipts").unwrap_or(&Value::Null)),
        verification,
        artifact_references: sanitize(object.get("tool_artifacts").unwrap_or(&Value::Null)),
        completion_reason,
        event_timeline: timeline,
        provenance: Provenance { first_event_checksum: first, final_event_checksum: final_checksum, report_fingerprint: String::new() },
    };
    let bytes = serde_json::to_vec(&report).map_err(|error| error.to_string())?;
    report.provenance.report_fingerprint = hex::encode(Sha256::digest(bytes));
    Ok(report)
}

fn sanitize(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(redact(text)),
        Value::Array(items) => Value::Array(items.iter().take(MAX_ITEMS).map(sanitize).collect()),
        Value::Object(object) => Value::Object(object.iter().map(|(key, value)| {
            (key.clone(), if secret_like(key) { Value::String("[REDACTED]".to_owned()) } else { sanitize(value) })
        }).collect::<Map<_, _>>()),
        other => other.clone(),
    }
}

fn redact(text: &str) -> String {
    let bounded = text.chars().take(MAX_STRING).collect::<String>();
    bounded.split_whitespace().map(|token| {
        if secret_like(token) || token.starts_with("sk-") || token.starts_with("ghp_") { "[REDACTED]" } else { token }
    }).collect::<Vec<_>>().join(" ")
}

fn secret_like(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("api_key") || lower.contains("apikey") || lower.contains("authorization") || lower.contains("password") || lower.contains("secret") || lower.contains("token=")
}

fn markdown(report: &AuditReport) -> String {
    let json = serde_json::to_value(report).unwrap_or(Value::Null);
    format!(
        "# Medusa Session Audit Report\n\n- Schema: `{}`\n- Session: `{}`\n- Status: `{}`\n- Completion reason: `{}`\n- Report fingerprint: `{}`\n\n## Objective\n\n{}\n\n## Audit data\n\n```json\n{}\n```\n",
        report.schema_version,
        report.session_id,
        report.status,
        report.completion_reason,
        report.provenance.report_fingerprint,
        report.objective.as_str().unwrap_or(""),
        serde_json::to_string_pretty(&json).unwrap_or_default(),
    )
}

fn option_value(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|arg| arg == name).and_then(|index| args.get(index + 1)).cloned()
}

fn session_path(repo: &Path, id: &str) -> PathBuf {
    repo.join(".medusa/sessions").join(format!("{id}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_values_are_redacted_and_arrays_are_bounded() {
        let value = serde_json::json!({"api_key": "sk-secret", "text": "token=private", "items": (0..200).collect::<Vec<_>>()});
        let clean = sanitize(&value);
        assert_eq!(clean["api_key"], "[REDACTED]");
        assert_eq!(clean["text"], "[REDACTED]");
        assert_eq!(clean["items"].as_array().unwrap().len(), MAX_ITEMS);
    }
}
