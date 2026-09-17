use std::collections::BTreeMap;

use medusa_execution_checkpoint::{ExecutionCheckpoint, ExecutionLog};
use medusa_testkit::resilience::{FaultPlan, FaultPoint, SMOKE_SEEDS, corruption_cases};

fn fingerprint(byte: u8) -> String {
    format!("{byte:02x}").repeat(32)
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
