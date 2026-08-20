use medusa_core::{CorrelationId, SessionId};
use medusa_protocol::{Actor, EventEnvelope, EventPayload};
use medusa_runtime::behavioral_outcome::{
    BehavioralTerminalStatus, behavioral_outcome_from_events,
};
use serde_json::json;
use time::OffsetDateTime;

fn events(payloads: Vec<EventPayload>) -> Vec<EventEnvelope> {
    let session_id = SessionId::new();
    let mut previous_hash = None;
    payloads
        .into_iter()
        .enumerate()
        .map(|(index, payload)| {
            let event = EventEnvelope::new(
                index as u64,
                session_id.clone(),
                Actor::Coordinator,
                CorrelationId::new(),
                payload,
                previous_hash.clone(),
                OffsetDateTime::from_unix_timestamp(1_700_000_000 + index as i64)
                    .expect("timestamp"),
            )
            .expect("event");
            previous_hash = Some(event.checksum.clone());
            event
        })
        .collect()
}

#[test]
fn model_claim_without_authoritative_receipts_is_not_success() {
    let journal = events(vec![
        EventPayload::SessionCreated {
            objective: "repair the repository".to_owned(),
        },
        EventPayload::AssistantMessageRecorded {
            message: json!({"text": "fixed; all tests pass"}),
        },
        EventPayload::SessionCompleted {
            report_ref: "report".to_owned(),
        },
    ]);

    let outcome =
        behavioral_outcome_from_events("session-a", Some("revision-a".to_owned()), &journal)
            .expect("outcome");

    assert!(!outcome.verified_success);
    assert_eq!(outcome.verification_passed, None);
    assert_eq!(
        outcome.terminal_status,
        BehavioralTerminalStatus::Inconclusive
    );
}

#[test]
fn verified_success_uses_durable_route_and_receipt_authority() {
    let journal = events(vec![
        EventPayload::SessionCreated {
            objective: "repair the repository".to_owned(),
        },
        EventPayload::ModelRequestStarted {
            provider: "minimax".to_owned(),
            model: "MiniMax-M2.7".to_owned(),
            request_id: Some("request-1".to_owned()),
            request_fingerprint: Some("request-fingerprint".to_owned()),
            manifest_ref: Some("manifest-1".to_owned()),
            attempt_ordinal: 1,
            parent_request_id: None,
        },
        EventPayload::ModelResponseReceived {
            response_id: Some("response-1".to_owned()),
            usage: json!({"input_tokens": 123, "output_tokens": 45}),
            request_id: Some("request-1".to_owned()),
            request_fingerprint: Some("request-fingerprint".to_owned()),
        },
        EventPayload::ProviderExecutionRecorded {
            status: json!({"cost_microusd": 17}),
        },
        EventPayload::FileTransactionCommitted {
            paths: vec!["src/lib.rs".to_owned()],
            rollback_ref: "rollback-1".to_owned(),
        },
        EventPayload::VerificationStarted {
            commands: vec!["cargo test".to_owned()],
        },
        EventPayload::VerificationCompleted {
            passed: true,
            evidence: vec!["verification-receipt-1".to_owned()],
        },
        EventPayload::IntegrationReceiptRecorded {
            receipt: json!({"receipt_id": "integration-receipt-1"}),
        },
        EventPayload::SessionCompleted {
            report_ref: "report".to_owned(),
        },
    ]);

    let outcome =
        behavioral_outcome_from_events("session-b", Some("revision-b".to_owned()), &journal)
            .expect("outcome");

    assert!(outcome.root_task_eligible);
    assert!(outcome.verified_success);
    assert_eq!(
        outcome.terminal_status,
        BehavioralTerminalStatus::VerifiedSuccess
    );
    assert_eq!(
        outcome.verification_receipt_ids,
        vec!["verification-receipt-1"]
    );
    assert_eq!(
        outcome.integration_receipt_ids,
        vec!["integration-receipt-1"]
    );
    assert_eq!(outcome.model_executions.len(), 1);
    assert_eq!(outcome.model_executions[0].provider, "minimax");
    assert_eq!(outcome.model_executions[0].model, "MiniMax-M2.7");
    assert!(outcome.model_executions[0].mutation_contribution);
    assert_eq!(
        outcome.model_executions[0].request_id.as_deref(),
        Some("request-1")
    );
    assert_eq!(
        outcome.model_executions[0].response_id.as_deref(),
        Some("response-1")
    );
    assert_eq!(outcome.observed_token_usage, Some(168));
    assert_eq!(outcome.monetary_cost_microunits, Some(17));
    assert_eq!(
        outcome.task_classification.intent,
        medusa_runtime::behavioral_outcome::BehavioralTaskIntent::BugFix
    );
    assert_eq!(outcome.task_classification.language_families, vec!["rust"]);
}

#[test]
fn task_aware_cohorts_keep_unknown_cost_and_detect_sustained_success_drift() {
    let success_events = events(vec![
        EventPayload::SessionCreated {
            objective: "fix Rust assertions".to_owned(),
        },
        EventPayload::FileTransactionCommitted {
            paths: vec!["src/lib.rs".to_owned()],
            rollback_ref: "rollback".to_owned(),
        },
        EventPayload::VerificationCompleted {
            passed: true,
            evidence: vec!["verified".to_owned()],
        },
        EventPayload::SessionCompleted {
            report_ref: "report".to_owned(),
        },
    ]);
    let failure_events = events(vec![
        EventPayload::SessionCreated {
            objective: "fix Rust assertions".to_owned(),
        },
        EventPayload::FileTransactionCommitted {
            paths: vec!["src/lib.rs".to_owned()],
            rollback_ref: "rollback".to_owned(),
        },
        EventPayload::VerificationCompleted {
            passed: false,
            evidence: vec!["failed".to_owned()],
        },
        EventPayload::SessionCompleted {
            report_ref: "report".to_owned(),
        },
    ]);
    let baseline = (0..16)
        .map(|index| {
            behavioral_outcome_from_events(&format!("baseline-{index}"), None, &success_events)
                .expect("baseline")
        })
        .collect::<Vec<_>>();
    let current = (0..16)
        .map(|index| {
            behavioral_outcome_from_events(&format!("current-{index}"), None, &failure_events)
                .expect("current")
        })
        .collect::<Vec<_>>();
    let baseline_report =
        medusa_runtime::behavioral_outcome::behavioral_metrics::build_cohort_reports(&baseline)
            .pop()
            .expect("baseline report");
    let current_report =
        medusa_runtime::behavioral_outcome::behavioral_metrics::build_cohort_reports(&current)
            .pop()
            .expect("current report");
    assert_eq!(baseline_report.cost_per_verified_success_microunits, None);
    assert_eq!(baseline_report.verified_success_rate_milli, Some(1_000));
    let drift =
        medusa_runtime::behavioral_outcome::behavioral_metrics::detect_verified_success_drift(
            &baseline_report,
            &current_report,
            medusa_runtime::behavioral_outcome::behavioral_metrics::DriftPolicy::default(),
        );
    assert_eq!(
        drift.classification,
        medusa_runtime::behavioral_outcome::behavioral_metrics::DriftClassification::Regression
    );
    assert!(drift.correlational_only);
}

#[test]
fn only_the_execution_preceding_mutation_gets_correctness_contribution() {
    let journal = events(vec![
        EventPayload::SessionCreated {
            objective: "repair the repository".to_owned(),
        },
        EventPayload::ModelRequestStarted {
            provider: "provider-a".to_owned(),
            model: "model-a".to_owned(),
            request_id: Some("request-a".to_owned()),
            request_fingerprint: Some("fingerprint-a".to_owned()),
            manifest_ref: Some("manifest-a".to_owned()),
            attempt_ordinal: 1,
            parent_request_id: None,
        },
        EventPayload::ModelResponseReceived {
            response_id: Some("response-a".to_owned()),
            usage: json!({"total_tokens": 10}),
            request_id: Some("request-a".to_owned()),
            request_fingerprint: Some("fingerprint-a".to_owned()),
        },
        EventPayload::ModelRequestStarted {
            provider: "provider-b".to_owned(),
            model: "model-b".to_owned(),
            request_id: Some("request-b".to_owned()),
            request_fingerprint: Some("fingerprint-b".to_owned()),
            manifest_ref: Some("manifest-b".to_owned()),
            attempt_ordinal: 1,
            parent_request_id: None,
        },
        EventPayload::ModelResponseReceived {
            response_id: Some("response-b".to_owned()),
            usage: json!({"total_tokens": 20}),
            request_id: Some("request-b".to_owned()),
            request_fingerprint: Some("fingerprint-b".to_owned()),
        },
        EventPayload::FileTransactionCommitted {
            paths: vec!["src/lib.rs".to_owned()],
            rollback_ref: "rollback".to_owned(),
        },
        EventPayload::VerificationCompleted {
            passed: true,
            evidence: vec!["verified".to_owned()],
        },
        EventPayload::IntegrationReceiptRecorded {
            receipt: json!({"id": "integrated"}),
        },
        EventPayload::SessionCompleted {
            report_ref: "report".to_owned(),
        },
    ]);

    let outcome = behavioral_outcome_from_events("session-route", None, &journal).expect("outcome");

    assert!(!outcome.model_executions[0].mutation_contribution);
    assert!(outcome.model_executions[1].mutation_contribution);
    assert_eq!(
        outcome
            .contributing_execution()
            .map(|execution| execution.model.as_str()),
        Some("model-b")
    );
    assert_eq!(outcome.observed_token_usage, Some(30));
}

#[test]
fn unmatched_response_request_id_is_not_attributed_to_latest_execution() {
    let journal = events(vec![
        EventPayload::SessionCreated {
            objective: "inspect provider history".to_owned(),
        },
        EventPayload::ModelRequestStarted {
            provider: "provider-a".to_owned(),
            model: "model-a".to_owned(),
            request_id: Some("request-a".to_owned()),
            request_fingerprint: Some("fingerprint-a".to_owned()),
            manifest_ref: None,
            attempt_ordinal: 1,
            parent_request_id: None,
        },
        EventPayload::ModelResponseReceived {
            response_id: Some("response-before-slice".to_owned()),
            usage: json!({"total_tokens": 999}),
            request_id: Some("request-before-slice".to_owned()),
            request_fingerprint: Some("fingerprint-before-slice".to_owned()),
        },
    ]);

    let outcome =
        behavioral_outcome_from_events("session-unmatched", None, &journal).expect("outcome");

    assert_eq!(outcome.model_executions.len(), 1);
    assert_eq!(outcome.model_executions[0].response_id, None);
    assert_eq!(outcome.model_executions[0].usage, None);
    assert_eq!(outcome.observed_token_usage, None);
}

#[test]
fn failed_verification_then_repair_preserves_first_pass_failure() {
    let journal = events(vec![
        EventPayload::SessionCreated {
            objective: "repair three assertions".to_owned(),
        },
        EventPayload::FileTransactionCommitted {
            paths: vec!["src/a.txt".to_owned()],
            rollback_ref: "rollback-1".to_owned(),
        },
        EventPayload::VerificationStarted {
            commands: vec!["verify".to_owned()],
        },
        EventPayload::VerificationCompleted {
            passed: false,
            evidence: vec!["verification-failed-1".to_owned()],
        },
        EventPayload::FileTransactionCommitted {
            paths: vec!["src/b.txt".to_owned(), "src/c.txt".to_owned()],
            rollback_ref: "rollback-2".to_owned(),
        },
        EventPayload::VerificationStarted {
            commands: vec!["verify".to_owned()],
        },
        EventPayload::VerificationCompleted {
            passed: true,
            evidence: vec!["verification-passed-2".to_owned()],
        },
        EventPayload::IntegrationReceiptRecorded {
            receipt: json!({"id": "integration-2"}),
        },
        EventPayload::SessionCompleted {
            report_ref: "report".to_owned(),
        },
    ]);

    let outcome = behavioral_outcome_from_events("session-c", None, &journal).expect("outcome");

    assert!(outcome.verified_success);
    assert_eq!(outcome.verification_attempts, 2);
    assert_eq!(outcome.failed_verification_attempts, 1);
    assert_eq!(outcome.mutation_count, 2);
    assert_eq!(outcome.verification_receipt_ids.len(), 2);
}

#[test]
fn recoverable_runtime_failure_before_final_verification_does_not_invalidate_success() {
    let journal = events(vec![
        EventPayload::SessionCreated {
            objective: "recover and verify".to_owned(),
        },
        EventPayload::RuntimeFailed {
            message: "transient crash".to_owned(),
        },
        EventPayload::RecoveryActionCompleted {
            receipt: json!({"id": "recovery-1"}),
        },
        EventPayload::VerificationCompleted {
            passed: true,
            evidence: vec!["verification-after-recovery".to_owned()],
        },
        EventPayload::SessionCompleted {
            report_ref: "recovered-report".to_owned(),
        },
    ]);

    let outcome =
        behavioral_outcome_from_events("session-recovered", None, &journal).expect("outcome");

    assert!(outcome.verified_success);
    assert_eq!(
        outcome.terminal_status,
        BehavioralTerminalStatus::VerifiedSuccess
    );
    assert_eq!(outcome.recovery_count, 1);
}

#[test]
fn delegated_worker_completion_cannot_become_root_verified_success() {
    let journal = events(vec![
        EventPayload::SessionCreated {
            objective:
                "Implement delegated task `implementation` inside this isolated Git worktree."
                    .to_owned(),
        },
        EventPayload::VerificationCompleted {
            passed: true,
            evidence: vec!["worker-verification".to_owned()],
        },
        EventPayload::SessionCompleted {
            report_ref: "worker-report".to_owned(),
        },
    ]);

    let outcome =
        behavioral_outcome_from_events("worker-session", None, &journal).expect("outcome");

    assert!(!outcome.root_task_eligible);
    assert!(!outcome.verified_success);
    assert_eq!(
        outcome.terminal_status,
        BehavioralTerminalStatus::Inconclusive
    );
}

#[test]
fn replay_is_deterministic_and_cancelled_runs_remain_visible() {
    let journal = events(vec![
        EventPayload::SessionCreated {
            objective: "long task".to_owned(),
        },
        EventPayload::CancellationRequested {
            source: "user".to_owned(),
        },
        EventPayload::CancellationCompleted,
    ]);

    let first =
        behavioral_outcome_from_events("session-d", Some("revision-d".to_owned()), &journal)
            .expect("first");
    let second =
        behavioral_outcome_from_events("session-d", Some("revision-d".to_owned()), &journal)
            .expect("second");

    assert_eq!(first, second);
    assert_eq!(first.terminal_status, BehavioralTerminalStatus::Cancelled);
    assert!(!first.verified_success);
    assert!(first.cancellation_requested);
}

#[test]
fn post_terminal_events_do_not_change_completed_outcome_identity() {
    let mut journal = events(vec![
        EventPayload::SessionCreated {
            objective: "complete once".to_owned(),
        },
        EventPayload::VerificationCompleted {
            passed: true,
            evidence: vec!["verified".to_owned()],
        },
        EventPayload::SessionCompleted {
            report_ref: "report".to_owned(),
        },
    ]);
    let before = behavioral_outcome_from_events("session-stable", None, &journal).expect("before");

    let terminal_hash = journal.last().expect("terminal event").checksum.clone();
    let event = EventEnvelope::new(
        journal.len() as u64,
        SessionId::new(),
        Actor::Coordinator,
        CorrelationId::new(),
        EventPayload::SessionReset {
            reason: "new task".to_owned(),
        },
        Some(terminal_hash),
        OffsetDateTime::from_unix_timestamp(1_700_000_100).expect("timestamp"),
    )
    .expect("post-terminal event");
    // Keep the event chain valid while deliberately using a new session identity, as a replayed
    // append can do when the original session is reopened.
    journal.push(event);

    let after = behavioral_outcome_from_events("session-stable", None, &journal).expect("after");

    assert_eq!(before.outcome_id, after.outcome_id);
    assert_eq!(before.source_event_ids, after.source_event_ids);
    assert_eq!(before.terminal_status, after.terminal_status);
}
