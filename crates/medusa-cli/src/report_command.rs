use std::{
    fs,
    path::{Path, PathBuf},
};

use clap::{Parser, error::ErrorKind};
use medusa_protocol::{EventEnvelope, validate_session_id};
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

const SCHEMA: &str = "medusa.session-audit/v1";
const MAX_STRING: usize = 4096;
const MAX_ITEMS: usize = 128;

#[derive(Parser)]
#[command(name = "medusa report", disable_version_flag = true)]
struct ReportArgs {
    session_id: String,
    #[arg(long, default_value = "markdown", value_parser = ["markdown", "md", "json"])]
    format: String,
    #[arg(long)]
    output: Option<PathBuf>,
}

struct PendingMutation {
    tool: String,
    paths: Vec<String>,
    denied: bool,
}

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

pub fn run(repo: &Path, args: &[String]) -> Result<(), String> {
    let options = match ReportArgs::try_parse_from(
        std::iter::once("medusa report").chain(args.iter().map(String::as_str)),
    ) {
        Ok(options) => options,
        Err(error) if error.kind() == ErrorKind::DisplayHelp => {
            print!("{error}");
            return Ok(());
        }
        Err(error) => {
            return Err(format!(
                "{error}\nusage: medusa report <session-id> [--format markdown|json] [--output PATH]"
            ));
        }
    };
    let session_id = &options.session_id;
    validate_session_id(session_id).map_err(str::to_owned)?;

    let path = session_path(repo, session_id);
    let bytes = fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let session: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    verify_event_chain(&session)?;
    let report = build_report(&session, session_id)?;
    let rendered = if options.format == "json" {
        serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
    } else {
        markdown(&report)
    };

    if let Some(output) = options.output {
        fs::write(&output, rendered)
            .map_err(|error| format!("write {}: {error}", output.display()))?;
    } else {
        println!("{rendered}");
    }
    Ok(())
}

fn verify_event_chain(session: &Value) -> Result<(), String> {
    let events = session
        .get("events")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let events: Vec<EventEnvelope> =
        serde_json::from_value(events).map_err(|error| format!("parse session events: {error}"))?;
    let mut previous_checksum: Option<&str> = None;
    let mut previous_sequence = 0;

    for event in &events {
        event
            .validate()
            .map_err(|error| format!("invalid event {}: {error}", event.sequence))?;
        if event.sequence <= previous_sequence {
            return Err(format!(
                "invalid event order: sequence {} follows {}",
                event.sequence, previous_sequence
            ));
        }
        if event.previous_hash.as_deref() != previous_checksum {
            return Err(format!(
                "invalid event chain at sequence {}: previous hash does not match",
                event.sequence
            ));
        }
        previous_sequence = event.sequence;
        previous_checksum = Some(&event.checksum);
    }
    Ok(())
}

fn build_report(session: &Value, requested_id: &str) -> Result<AuditReport, String> {
    let object = session
        .as_object()
        .ok_or_else(|| "session root must be an object".to_owned())?;
    let events = object
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut routes = Vec::new();
    let mut orchestration = Vec::new();
    let mut files_changed = Vec::new();
    let mut requested = Vec::new();
    let mut executed = Vec::new();
    let mut pending_mutations: Vec<PendingMutation> = Vec::new();
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
        let event_type = payload
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let data = payload.get("data").cloned().unwrap_or(Value::Null);
        match event_type {
            "model_request_started" => routes.push(sanitize(&data)),
            "plan_created" | "plan_updated" => orchestration.push(sanitize(&payload)),
            "tool_call_requested" => {
                requested.push(sanitize(&data));
                let tool = data.get("tool").and_then(Value::as_str).unwrap_or_default();
                let paths = mutation_paths(tool, data.get("arguments").unwrap_or(&Value::Null));
                if !paths.is_empty() {
                    pending_mutations.push(PendingMutation {
                        tool: tool.to_owned(),
                        paths,
                        denied: false,
                    });
                }
            }
            "tool_call_denied" => {
                containment.push(sanitize(&data));
                if let Some(pending) = pending_mutations.iter_mut().find(|pending| {
                    data.get("tool").and_then(Value::as_str) == Some(pending.tool.as_str())
                        && !pending.denied
                }) {
                    pending.denied = true;
                }
            }
            "approval_decision_recorded" => {
                let decision = data.get("decision").unwrap_or(&Value::Null);
                let tool = decision.get("tool").and_then(Value::as_str);
                let outcome = decision
                    .pointer("/receipt/decision")
                    .and_then(Value::as_str);
                if let Some(position) = pending_mutations
                    .iter()
                    .position(|pending| pending.denied && Some(pending.tool.as_str()) == tool)
                {
                    match outcome {
                        Some("approved") => pending_mutations[position].denied = false,
                        Some("denied" | "expired" | "invalidated") => {
                            pending_mutations.remove(position);
                        }
                        _ => {}
                    }
                }
            }
            "tool_execution_completed" => {
                executed.push(sanitize(&data));
                let tool = data.get("tool").and_then(Value::as_str).unwrap_or_default();
                let succeeded = data
                    .get("exit_code")
                    .and_then(Value::as_i64)
                    .is_none_or(|code| code == 0);
                if let Some(position) = pending_mutations
                    .iter()
                    .position(|pending| pending.tool == tool && (!succeeded || !pending.denied))
                {
                    let pending = pending_mutations.remove(position);
                    if succeeded && !pending.denied {
                        for path in pending.paths {
                            push_unique(&mut files_changed, path);
                        }
                    }
                }
            }
            "file_transaction_committed" => {
                if let Some(paths) = data.get("paths").and_then(Value::as_array) {
                    for path in paths.iter().filter_map(Value::as_str) {
                        push_unique(&mut files_changed, path.to_owned());
                    }
                }
            }
            "checkpoint_created" => checkpoints.push(sanitize(&data)),
            "verification_started" | "verification_completed" => {
                verification.push(sanitize(&payload));
            }
            "session_failed" | "session_paused" | "session_resumed" | "session_state_changed" => {
                failures.push(sanitize(&payload));
            }
            "session_completed" => completion_reason = "verified_completion".to_owned(),
            _ => {}
        }
        timeline.push(sanitize(event));
    }

    let first = events
        .first()
        .and_then(|event| event.get("checksum"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let final_checksum = events
        .last()
        .and_then(|event| event.get("checksum"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut report = AuditReport {
        schema_version: SCHEMA,
        session_id: object
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(requested_id)
            .to_owned(),
        objective: sanitize(object.get("objective").unwrap_or(&Value::Null)),
        repository: sanitize(object.get("repo").unwrap_or(&Value::Null)),
        created_at: sanitize(object.get("created_at").unwrap_or(&Value::Null)),
        updated_at: sanitize(object.get("updated_at").unwrap_or(&Value::Null)),
        status: if object.get("completed").and_then(Value::as_bool) == Some(true) {
            "completed"
        } else {
            "incomplete"
        }
        .to_owned(),
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
        provenance: Provenance {
            first_event_checksum: first,
            final_event_checksum: final_checksum,
            report_fingerprint: String::new(),
        },
    };
    let bytes = serde_json::to_vec(&report).map_err(|error| error.to_string())?;
    report.provenance.report_fingerprint = hex::encode(Sha256::digest(bytes));
    Ok(report)
}

fn mutation_paths(tool: &str, arguments: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    if !matches!(
        tool,
        "fs_write" | "patch_apply" | "fs_rename" | "fs_move" | "fs_delete"
    ) {
        return paths;
    }
    for key in ["path", "file", "destination", "to", "source", "from"] {
        if let Some(path) = arguments.get(key).and_then(Value::as_str) {
            push_unique(&mut paths, path.to_owned());
        }
    }
    if let Some(items) = arguments.get("paths").and_then(Value::as_array) {
        for path in items.iter().filter_map(Value::as_str) {
            push_unique(&mut paths, path.to_owned());
        }
    }
    paths
}

fn push_unique(items: &mut Vec<String>, value: String) {
    if !items.contains(&value) {
        items.push(value);
    }
}

fn sanitize(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(redact(text)),
        Value::Array(items) => {
            let mut redact_next = 0;
            Value::Array(
                items
                    .iter()
                    .take(MAX_ITEMS)
                    .map(|item| {
                        let credential = redact_next > 0;
                        match item {
                            Value::String(text) => Value::String(if credential {
                                redact_next -= 1;
                                "[REDACTED]".to_owned()
                            } else if (text.contains('=') && secret_like(text))
                                || text.starts_with("sk-")
                                || text.starts_with("ghp_")
                            {
                                "[REDACTED]".to_owned()
                            } else {
                                redact_with_state(text, &mut redact_next)
                            }),
                            _ if credential => {
                                redact_next -= 1;
                                Value::String("[REDACTED]".to_owned())
                            }
                            _ => sanitize(item),
                        }
                    })
                    .collect(),
            )
        }
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        if secret_like(key) {
                            Value::String("[REDACTED]".to_owned())
                        } else {
                            sanitize(value)
                        },
                    )
                })
                .collect::<Map<_, _>>(),
        ),
        other => other.clone(),
    }
}

fn redact(text: &str) -> String {
    redact_with_state(text, &mut 0)
}

fn redact_with_state(text: &str, redact_next: &mut u8) -> String {
    let bounded = text.chars().take(MAX_STRING).collect::<String>();
    let tokens = bounded.split_whitespace().collect::<Vec<_>>();
    let mut redacted = Vec::with_capacity(tokens.len());
    for token in tokens {
        let lower = token.to_ascii_lowercase();
        if *redact_next > 0 {
            redacted.push("[REDACTED]");
            *redact_next -= 1;
            continue;
        }
        if lower == "bearer" {
            redacted.push("[REDACTED]");
            *redact_next = 1;
        } else if secret_like(token) || token.starts_with("sk-") || token.starts_with("ghp_") {
            redacted.push("[REDACTED]");
            *redact_next =
                if token.contains('=') || token.starts_with("sk-") || token.starts_with("ghp_") {
                    0
                } else if lower.contains("authorization") {
                    2
                } else {
                    1
                };
        } else {
            redacted.push(token);
        }
    }
    redacted.join(" ")
}

fn secret_like(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("api_key")
        || lower.contains("apikey")
        || lower.contains("authorization")
        || lower.contains("password")
        || lower.contains("passwd")
        || lower.contains("secret")
        || lower.contains("token=")
        || lower == "--token"
        || lower == "--password"
        || lower == "-p"
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

fn session_path(repo: &Path, id: &str) -> PathBuf {
    let primary = repo.join(".medusa/sessions").join(format!("{id}.json"));
    if primary.is_file() {
        primary
    } else {
        fallback_session_root(repo).join(format!("{id}.json"))
    }
}

fn fallback_session_root(repo: &Path) -> PathBuf {
    let root = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir);
    root.join("Medusa/sessions").join(repository_key(repo))
}

fn repository_key(repo: &Path) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in repo.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_values_and_following_credentials_are_redacted() {
        let value = serde_json::json!({
            "api_key": "sk-secret",
            "password_command": "curl --password hunter2 Authorization: Bearer abc123",
            "items": (0..200).collect::<Vec<_>>()
        });
        let clean = sanitize(&value);
        assert_eq!(clean["api_key"], "[REDACTED]");
        let command = clean["password_command"].as_str().unwrap();
        assert!(!command.contains("hunter2"));
        assert!(!command.contains("abc123"));
        assert_eq!(clean["items"].as_array().unwrap().len(), MAX_ITEMS);
    }

    #[test]
    fn successful_mutation_paths_are_derived_from_requested_tools() {
        assert_eq!(
            mutation_paths("fs_write", &serde_json::json!({"path": "src/lib.rs"})),
            vec!["src/lib.rs"]
        );
        assert!(mutation_paths("fs_read", &serde_json::json!({"path": "src/lib.rs"})).is_empty());
    }
}
