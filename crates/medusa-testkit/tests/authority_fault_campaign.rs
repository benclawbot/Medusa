use std::collections::BTreeMap;

use medusa_execution_checkpoint::{ExecutionCheckpoint, ExecutionLog};
use medusa_testkit::resilience::{corruption_cases, FaultPlan, FaultPoint, SMOKE_SEEDS};
use medusa_transaction_coordinator::{
    Participant, TransactionCoordinator, TransactionPhase, Vote,
};

fn fingerprint(byte: u8) -> String {
    format!("{byte:02x}").repeat(32)
}

#[test]
fn fault_plans_drive_transaction_promotion_authority() {
    for (case, seed) in SMOKE_SEEDS.into_iter().enumerate() {
        let plan = FaultPlan::new(seed, 3);
        let transaction_id = format!("txn-{case}");
        let mut coordinator = TransactionCoordinator::default();
        coordinator
            .create(
                &transaction_id,
                format!("exec-{case}"),
                1,
                vec![Participant {
                    worker_id: "worker-1".to_owned(),
                    lease_epoch: 7,
                    intent_fingerprint: fingerprint(1),
                }],
                fingerprint(2),
            )
            .expect("create transaction");
        coordinator
            .transition(&transaction_id, TransactionPhase::Resolving)
            .expect("resolve");
        coordinator
            .transition(&transaction_id, TransactionPhase::Preparing)
            .expect("prepare");
        coordinator
            .vote(
                &transaction_id,
                Vote::Prepared {
                    worker_id: "worker-1".to_owned(),
                    lease_epoch: 7,
                },
            )
            .expect("prepared vote");

        if plan.injects(FaultPoint::BeforeCandidatePromotion, 0) {
            coordinator
                .transition(&transaction_id, TransactionPhase::Aborting)
                .expect("abort before promotion");
            coordinator
                .transition(&transaction_id, TransactionPhase::Aborted)
                .expect("finish abort");
        } else {
            coordinator
                .transition(&transaction_id, TransactionPhase::Prepared)
                .expect("prepared");
            if plan.injects(FaultPoint::AfterCandidatePromotion, 0) {
                coordinator
                    .transition(&transaction_id, TransactionPhase::Aborting)
                    .expect("abort after promotion interruption");
                coordinator
                    .transition(&transaction_id, TransactionPhase::Aborted)
                    .expect("finish abort");
            } else {
                coordinator
                    .transition(&transaction_id, TransactionPhase::Committing)
                    .expect("commit start");
                coordinator
                    .transition(&transaction_id, TransactionPhase::Committed)
                    .expect("commit finish");
            }
        }

        coordinator.verify(&transaction_id).expect("fingerprint valid");
        let phase = &coordinator.record(&transaction_id).expect("record").phase;
        assert!(matches!(
            phase,
            TransactionPhase::Aborted | TransactionPhase::Committed
        ));
    }
}

#[test]
fn fault_plans_drive_checkpoint_snapshot_recovery() {
    for seed in SMOKE_SEEDS {
        let plan = FaultPlan::new(seed, 3);
        let mut log = ExecutionLog::new(format!("exec-{seed:016x}")).expect("log");
        let event_fingerprint = log
            .append_event("tool_completed", fingerprint(3))
            .expect("event")
            .fingerprint
            .clone();

        if plan.injects(FaultPoint::BeforeSnapshotPersist, 0) {
            assert_eq!(log.replay_tail().expect("tail").len(), 1);
            continue;
        }

        let checkpoint = ExecutionCheckpoint::new(
            log.execution_id.clone(),
            1,
            fingerprint(4),
            fingerprint(5),
            Some(event_fingerprint),
            BTreeMap::new(),
        )
        .expect("checkpoint");
        log.add_checkpoint(checkpoint).expect("persist checkpoint");
        let encoded = serde_json::to_vec(&log).expect("encode checkpoint log");

        if plan.injects(FaultPoint::AfterSnapshotPersist, 0) {
            let truncated = corruption_cases(&encoded, seed, encoded.len())
                .into_iter()
                .next()
                .expect("truncation case");
            assert!(serde_json::from_slice::<ExecutionLog>(&truncated).is_err());
        }

        let recovered: ExecutionLog = serde_json::from_slice(&encoded).expect("recover log");
        recovered.verify().expect("recovered log verifies");
        assert!(recovered.replay_tail().expect("tail").is_empty());
    }
}
