use std::{fs, thread, time::Duration};

use medusa_agent::AgentEngine;
use medusa_config::Config;
use medusa_core::MedusaResult;
use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};
use medusa_recovery_coordinator::RecoveryOperation;
use medusa_runtime::{RuntimeController, RuntimeEvent, commands::SlashCommand};
use tempfile::tempdir;

struct UnusedProvider;

impl ModelProvider for UnusedProvider {
    fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {
        unreachable!("recovery coverage does not call the provider")
    }
}

fn recovery_command(task: Option<&str>) -> SlashCommand {
    SlashCommand::Skill {
        selector: "recovery".to_owned(),
        task: task.map(str::to_owned),
    }
}

fn collect_until(
    controller: &RuntimeController,
    complete: impl Fn(&RuntimeEvent) -> bool,
) -> Vec<RuntimeEvent> {
    let mut events = Vec::new();
    for _ in 0..1_500 {
        match controller.try_event() {
            Ok(Some(event)) => {
                let done = complete(&event);
                events.push(event);
                if done {
                    return events;
                }
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => panic!("runtime stopped: {error}"),
        }
    }
    panic!("timed out waiting for recovery event; observed {events:#?}")
}

fn run_until(
    controller: &RuntimeController,
    task: Option<&str>,
    complete: impl Fn(&RuntimeEvent) -> bool,
) -> Vec<RuntimeEvent> {
    controller
        .run_command(recovery_command(task))
        .expect("dispatch recovery command");
    collect_until(controller, complete)
}

fn create_session(repo: &std::path::Path) -> String {
    AgentEngine::new(UnusedProvider, Config::default())
        .create_session(repo, "Recovery coverage".to_owned())
        .expect("create durable session")
        .id
        .to_string()
}

fn write_recovery_record(repo: &std::path::Path, session_id: &str) {
    let directory = repo.join(".medusa/recovery");
    fs::create_dir_all(&directory).expect("create recovery directory");
    let record = serde_json::json!({
        "session_id": session_id,
        "last_durable_step": "implement",
        "interrupted_operation": "cargo test",
        "current_repository_fingerprint": "b".repeat(64),
        "verification": "Incomplete",
        "approvals_must_be_reestablished": true,
        "containment_must_be_reestablished": false,
        "checkpoints": [{
            "id": "checkpoint-1",
            "sequence": 1,
            "created_at_unix_ms": 1_700_000_000_000_i64,
            "task_step": "implement",
            "reason": "durable progress",
            "repository_fingerprint": "a".repeat(64),
            "verification": "Incomplete",
            "provenance": "execution-checkpoint/v1",
            "integrity_verified": true
        }],
        "selected_preview": null
    });
    fs::write(
        directory.join("00000000-coverage.json"),
        serde_json::to_vec_pretty(&record).expect("serialize recovery record"),
    )
    .expect("write recovery record");
}

#[test]
fn recovery_commands_execute_safe_actions_and_reject_restore_without_preview() {
    let repo = tempdir().expect("temporary repository");
    let session_id = create_session(repo.path());
    let controller =
        RuntimeController::start_with_config(repo.path().to_path_buf(), Config::default());
    collect_until(
        &controller,
        |event| matches!(event, RuntimeEvent::Notice { title, .. } if title == "Runtime capabilities"),
    );
    write_recovery_record(repo.path(), &session_id);

    let operations = [
        ("inspect", RecoveryOperation::Inspect),
        ("resume", RecoveryOperation::Resume),
        ("verify", RecoveryOperation::RetryVerification),
        ("retry-verification", RecoveryOperation::RetryVerification),
        ("abandon", RecoveryOperation::Abandon),
    ];
    for (task, expected_operation) in operations {
        write_recovery_record(repo.path(), &session_id);
        let events = run_until(&controller, Some(task), |event| {
            matches!(event, RuntimeEvent::RecoveryCompleted(_))
        });
        let receipt = events
            .iter()
            .find_map(|event| match event {
                RuntimeEvent::RecoveryCompleted(receipt) => Some(receipt),
                _ => None,
            })
            .expect("recovery completion receipt");
        assert_eq!(receipt.record.session_id, session_id);
        assert_eq!(receipt.record.operation, expected_operation);
        assert!(receipt.record.verify());
        assert!(receipt.audit_path.is_file());
    }

    let restore = run_until(
        &controller,
        Some("restore checkpoint-1"),
        |event| matches!(event, RuntimeEvent::Notice { title, .. } if title == "Recovery action failed closed"),
    );
    assert!(restore.iter().any(
        |event| matches!(event, RuntimeEvent::Notice { title, .. } if title == "Recovery action failed closed")
    ));

    let invalid = run_until(
        &controller,
        Some("invent"),
        |event| matches!(event, RuntimeEvent::Notice { title, .. } if title == "Recovery action failed closed"),
    );
    assert!(invalid.iter().any(|event| {
        matches!(
            event,
            RuntimeEvent::Notice { title, details }
                if title == "Recovery action failed closed"
                    && details.iter().any(|detail| detail.starts_with("usage:"))
        )
    }));

    drop(controller);
}

#[test]
fn recovery_discovery_reports_missing_and_corrupt_records_without_panicking() {
    let repo = tempdir().expect("temporary repository");
    let controller =
        RuntimeController::start_with_config(repo.path().to_path_buf(), Config::default());
    collect_until(&controller, |event| {
        matches!(event, RuntimeEvent::Settings { .. })
    });

    let missing = run_until(
        &controller,
        Some("inspect"),
        |event| matches!(event, RuntimeEvent::Notice { title, .. } if title == "Recovery action failed closed"),
    );
    assert!(missing.iter().any(|event| {
        matches!(
            event,
            RuntimeEvent::Notice { title, details }
                if title == "Recovery action failed closed"
                    && details.iter().any(|detail| detail.contains("no recoverable session"))
        )
    }));

    let directory = repo.path().join(".medusa/recovery");
    fs::create_dir_all(&directory).expect("create recovery directory");
    fs::write(directory.join("corrupt.json"), b"not-json").expect("write corrupt record");
    let corrupt = run_until(
        &controller,
        Some("inspect"),
        |event| matches!(event, RuntimeEvent::Notice { title, .. } if title == "Recovery action failed closed"),
    );
    assert!(corrupt.iter().any(|event| {
        matches!(
            event,
            RuntimeEvent::Notice { title, details }
                if title == "Recovery action failed closed"
                    && details.iter().any(|detail| detail.contains("record is corrupt"))
        )
    }));

    drop(controller);
}
