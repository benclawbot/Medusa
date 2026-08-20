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
    assert_eq!(
        outcome.model_executions[0].request_id.as_deref(),
        Some("request-1")
    );
    assert_eq!(
        outcome.model_executions[0].response_id.as_deref(),
        Some("response-1")
    );
    assert_eq!(outcome.monetary_cost_microunits, None);
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
