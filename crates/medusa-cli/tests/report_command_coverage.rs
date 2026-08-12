use std::fs;

use medusa_cli::report_command;

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
    write_session(repository.path(), "session-audit", true);

    let json_path = repository.path().join("audit.json");
    report_command::run(
        repository.path(),
        &[
            "session-audit".to_owned(),
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
    assert_eq!(report["session_id"], "session-audit");
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
            "session-audit".to_owned(),
            "--format".to_owned(),
            "markdown".to_owned(),
            "--output".to_owned(),
            markdown_path.to_string_lossy().into_owned(),
        ],
    )
    .expect("Markdown report");
    let markdown = fs::read_to_string(markdown_path).expect("Markdown report text");
    assert!(markdown.contains("# Medusa Session Audit Report"));
    assert!(markdown.contains("session-audit"));
    assert!(markdown.contains("Produce a redacted audit report"));
}

#[test]
fn report_command_rejects_invalid_invocations_without_mutating_session() {
    let repository = tempfile::tempdir().expect("repository");
    write_session(repository.path(), "session-open", false);

    let missing_id = report_command::run(repository.path(), &[]).expect_err("session id required");
    assert!(missing_id.contains("usage: medusa report"));

    let invalid_format = report_command::run(
        repository.path(),
        &[
            "session-open".to_owned(),
            "--format".to_owned(),
            "xml".to_owned(),
        ],
    )
    .expect_err("invalid format rejected");
    assert!(invalid_format.contains("--format must be markdown or json"));

    let missing_session = report_command::run(
        repository.path(),
        &[
            "does-not-exist".to_owned(),
            "--format".to_owned(),
            "json".to_owned(),
        ],
    )
    .expect_err("missing session rejected");
    assert!(missing_session.contains("read"));
}
