use std::fs;

use medusa_cli::report_command;
use medusa_core::{CorrelationId, SessionId};
use medusa_protocol::{Actor, EventEnvelope, EventPayload};

fn report_events(repository: &std::path::Path, payloads: Vec<EventPayload>) -> serde_json::Value {
    let id = SessionId::new();
    let mut events = Vec::<EventEnvelope>::new();
    for payload in payloads {
        events.push(
            EventEnvelope::new(
                events.len() as u64 + 1,
                id.clone(),
                Actor::Coordinator,
                CorrelationId::new(),
                payload,
                events.last().map(|event| event.checksum.clone()),
                serde_json::from_value(serde_json::json!([2026, 276, 0, 0, 0, 0, 0, 0, 0]))
                    .expect("timestamp"),
            )
            .expect("checksummed event"),
        );
    }
    write_session(repository, id.as_str(), false);
    let session_path = repository.join(format!(".medusa/sessions/{id}.json"));
    let mut session: serde_json::Value =
        serde_json::from_slice(&fs::read(&session_path).expect("session")).expect("session JSON");
    session["events"] = serde_json::to_value(events).expect("events JSON");
    fs::write(
        session_path,
        serde_json::to_vec(&session).expect("serialize session"),
    )
    .expect("write events");
    let output = repository.join("report.json");
    report_command::run(
        repository,
        &[
            id.to_string(),
            "--format".into(),
            "json".into(),
            "--output".into(),
            output.to_string_lossy().into_owned(),
        ],
    )
    .expect("report");
    serde_json::from_slice(&fs::read(output).expect("report bytes")).expect("report JSON")
}

#[test]
fn report_redacts_credentials_across_argument_array_entries() {
    let repository = tempfile::tempdir().expect("repository");
    let report = report_events(
        repository.path(),
        vec![EventPayload::ToolCallRequested {
            tool: "shell_run".into(),
            arguments: serde_json::json!({"program":"example", "args":[
                "--token", "fake-token-value", "--password", "fake multi word credential",
                "Authorization:", "Bearer", "fake-bearer-value", "ordinary-argument"
            ]}),
        }],
    );
    let serialized = serde_json::to_string(&report).expect("serialize report");
    for credential in [
        "fake-token-value",
        "fake multi word credential",
        "fake-bearer-value",
    ] {
        assert!(
            !serialized.contains(credential),
            "credential leaked: {credential}"
        );
    }
    assert!(serialized.contains("ordinary-argument"));
}

#[test]
fn report_does_not_attribute_success_to_a_denied_mutation() {
    let repository = tempfile::tempdir().expect("repository");
    let report = report_events(
        repository.path(),
        vec![
            EventPayload::ToolCallRequested {
                tool: "fs_write".into(),
                arguments: serde_json::json!({"path":"denied.txt"}),
            },
            EventPayload::ToolCallDenied {
                tool: "fs_write".into(),
                reason: "approval required".into(),
            },
            EventPayload::ApprovalDecisionRecorded {
                decision: serde_json::json!({"tool":"fs_write", "receipt":{"decision":"denied"}}),
            },
            EventPayload::ToolCallRequested {
                tool: "fs_write".into(),
                arguments: serde_json::json!({"path":"allowed.txt"}),
            },
            EventPayload::ToolExecutionCompleted {
                tool: "fs_write".into(),
                exit_code: Some(0),
            },
        ],
    );
    assert_eq!(report["files_changed"], serde_json::json!(["allowed.txt"]));
}

#[test]
fn report_keeps_pending_approval_distinct_from_a_later_successful_request() {
    let repository = tempfile::tempdir().expect("repository");
    let report = report_events(
        repository.path(),
        vec![
            EventPayload::ToolCallRequested {
                tool: "fs_write".into(),
                arguments: serde_json::json!({"path":"pending.txt"}),
            },
            EventPayload::ToolCallDenied {
                tool: "fs_write".into(),
                reason: "approval required".into(),
            },
            EventPayload::ToolCallRequested {
                tool: "fs_write".into(),
                arguments: serde_json::json!({"path":"allowed.txt"}),
            },
            EventPayload::ToolExecutionCompleted {
                tool: "fs_write".into(),
                exit_code: Some(0),
            },
        ],
    );
    assert_eq!(report["files_changed"], serde_json::json!(["allowed.txt"]));
}

#[test]
fn report_includes_an_explicitly_approved_mutation() {
    let repository = tempfile::tempdir().expect("repository");
    let report = report_events(
        repository.path(),
        vec![
            EventPayload::ToolCallRequested {
                tool: "fs_write".into(),
                arguments: serde_json::json!({"path":"approved.txt"}),
            },
            EventPayload::ToolCallDenied {
                tool: "fs_write".into(),
                reason: "approval required".into(),
            },
            EventPayload::ApprovalDecisionRecorded {
                decision: serde_json::json!({"tool":"fs_write", "receipt":{"decision":"approved"}}),
            },
            EventPayload::ToolExecutionCompleted {
                tool: "fs_write".into(),
                exit_code: Some(0),
            },
        ],
    );
    assert_eq!(report["files_changed"], serde_json::json!(["approved.txt"]));
}

#[test]
fn report_parses_options_before_the_session_id_and_rejects_missing_values() {
    let repository = tempfile::tempdir().expect("repository");
    let id = SessionId::new().to_string();
    write_session(repository.path(), &id, false);
    let output = repository.path().join("options.json");
    report_command::run(
        repository.path(),
        &[
            "--format".into(),
            "json".into(),
            "--output".into(),
            output.to_string_lossy().into_owned(),
            id.clone(),
        ],
    )
    .expect("options before positional ID");
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output).expect("output")).expect("JSON");
    assert_eq!(report["session_id"], id);
    for option in ["--format", "--output"] {
        assert!(report_command::run(repository.path(), &[id.clone(), option.into()]).is_err());
    }
    assert!(report_command::run(repository.path(), &[id, "--unknown".into()]).is_err());
}

#[test]
fn report_rejects_traversal_and_absolute_session_paths() {
    let directory = tempfile::tempdir().expect("directory");
    let repository = directory.path().join("repo");
    fs::create_dir_all(repository.join(".medusa/sessions")).expect("sessions");
    let outside = directory.path().join("outside.json");
    fs::write(&outside, br#"{"events":[],"objective":"outside"}"#).expect("outside fixture");
    for id in [
        "../../../outside".to_owned(),
        outside.with_extension("").to_string_lossy().into_owned(),
        "..\\..\\outside".to_owned(),
        "C:\\outside".to_owned(),
    ] {
        let output = directory.path().join("unsafe-report.json");
        assert!(
            report_command::run(
                &repository,
                &[
                    id.clone(),
                    "--output".into(),
                    output.to_string_lossy().into_owned()
                ]
            )
            .is_err(),
            "unsafe session ID accepted: {id}"
        );
        assert!(!output.exists(), "unsafe report was written");
    }
}

fn write_session(repo: &std::path::Path, id: &str, completed: bool) {
    let directory = repo.join(".medusa/sessions");
    fs::create_dir_all(&directory).expect("session directory");
    let session = serde_json::json!({
        "id": id,
        "objective": "Produce a redacted audit report",
        "repo": repo.to_string_lossy(),
        "created_at": "2026-08-12T12:00:00Z",
        "updated_at": "2026-08-12T12:01:00Z",
        "completed": completed,
        "turn": 2,
        "plan": {"summary": "audit"},
        "approval_receipts": [],
        "rollback_receipts": [],
        "tool_artifacts": [],
        "events": []
    });
    fs::write(
        directory.join(format!("{id}.json")),
        serde_json::to_vec_pretty(&session).expect("serialize session"),
    )
    .expect("write session");
}

#[test]
fn report_command_renders_json_and_markdown_from_durable_session() {
    let repository = tempfile::tempdir().expect("repository");
    write_session(repository.path(), "ses-01K00000000000000000000001", true);

    let json_path = repository.path().join("audit.json");
    report_command::run(
        repository.path(),
        &[
            "ses-01K00000000000000000000001".to_owned(),
            "--format".to_owned(),
            "json".to_owned(),
            "--output".to_owned(),
            json_path.to_string_lossy().into_owned(),
        ],
    )
    .expect("JSON report");
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(&json_path).expect("JSON report bytes"))
            .expect("JSON report value");
    assert_eq!(report["schema_version"], "medusa.session-audit/v1");
    assert_eq!(report["session_id"], "ses-01K00000000000000000000001");
    assert_eq!(report["status"], "completed");
    assert_eq!(report["completion_reason"], "completed");
    assert_eq!(report["files_changed"], serde_json::json!([]));
    assert!(
        report["provenance"]["report_fingerprint"]
            .as_str()
            .is_some_and(|fingerprint| fingerprint.len() == 64)
    );

    let markdown_path = repository.path().join("audit.md");
    report_command::run(
        repository.path(),
        &[
            "ses-01K00000000000000000000001".to_owned(),
            "--format".to_owned(),
            "markdown".to_owned(),
            "--output".to_owned(),
            markdown_path.to_string_lossy().into_owned(),
        ],
    )
    .expect("Markdown report");
    let markdown = fs::read_to_string(markdown_path).expect("Markdown report text");
    assert!(markdown.contains("# Medusa Session Audit Report"));
    assert!(markdown.contains("ses-01K00000000000000000000001"));
    assert!(markdown.contains("Produce a redacted audit report"));
}

#[test]
fn report_command_rejects_invalid_invocations_without_mutating_session() {
    let repository = tempfile::tempdir().expect("repository");
    write_session(repository.path(), "ses-01K00000000000000000000002", false);

    let missing_id = report_command::run(repository.path(), &[]).expect_err("session id required");
    assert!(missing_id.contains("usage: medusa report"));

    let invalid_format = report_command::run(
        repository.path(),
        &[
            "ses-01K00000000000000000000002".to_owned(),
            "--format".to_owned(),
            "xml".to_owned(),
        ],
    )
    .expect_err("invalid format rejected");
    assert!(invalid_format.contains("--format") && invalid_format.contains("xml"));

    let missing_session = report_command::run(
        repository.path(),
        &[
            "ses-01K00000000000000000000003".to_owned(),
            "--format".to_owned(),
            "json".to_owned(),
        ],
    )
    .expect_err("missing session rejected");
    assert!(missing_session.contains("read"));
}
