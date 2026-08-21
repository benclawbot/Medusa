use medusa_runtime::behavioral_health::{
    BEHAVIORAL_HEALTH_SCHEMA_VERSION, BehavioralHealthStatus, build_behavioral_health_snapshot,
};

#[test]
fn runtime_exports_shared_health_contract_without_overclaiming_evidence() {
    let snapshot = build_behavioral_health_snapshot(None, &[], None, None, None, None, None, None);

    assert_eq!(snapshot.schema_version, BEHAVIORAL_HEALTH_SCHEMA_VERSION);
    assert_eq!(
        snapshot.status,
        BehavioralHealthStatus::InsufficientEvidence
    );
    assert!(snapshot.verified_success_rate_milli.is_none());
    assert!(snapshot.cost_per_verified_success_microunits.is_none());
}
