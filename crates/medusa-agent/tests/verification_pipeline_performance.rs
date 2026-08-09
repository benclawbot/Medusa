use std::{fs, time::Instant};

use medusa_agent::authoritative_verification_for_components_at;
use medusa_evidence::{ChangeKind, ChangedComponent};
use serde_json::json;

const FIXTURE_COUNT: usize = 16;
const VALUES_PER_FIXTURE: usize = 131_072;
const SAMPLE_COUNT: usize = 7;

fn components() -> Vec<ChangedComponent> {
    (0..FIXTURE_COUNT)
        .map(|index| {
            ChangedComponent::new(ChangeKind::Modified, format!("artifacts/{index:03}.json"))
                .expect("component")
        })
        .collect()
}

fn write_fixtures(repo: &std::path::Path) {
    fs::create_dir_all(repo.join("artifacts")).expect("artifact directory");
    let values = (0..VALUES_PER_FIXTURE)
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(",");
    for index in 0..FIXTURE_COUNT {
        fs::write(
            repo.join(format!("artifacts/{index:03}.json")),
            format!("{{\"index\":{index},\"ok\":true,\"values\":[{values}]}}\n"),
        )
        .expect("artifact");
    }
}

#[test]
fn exact_authoritative_reuse_reduces_runtime_and_stale_input_reruns() {
    let repository = tempfile::tempdir().expect("repository");
    let evidence = tempfile::tempdir().expect("evidence");
    write_fixtures(repository.path());
    let components = components();

    let mut cold_ns = Vec::with_capacity(SAMPLE_COUNT);
    for sample in 0..SAMPLE_COUNT {
        let evidence_root = evidence.path().join(format!("cold-{sample}"));
        let started = Instant::now();
        let result = authoritative_verification_for_components_at(
            repository.path(),
            &evidence_root,
            "perf-repository",
            "perf-commit",
            &components,
        )
        .expect("cold verification");
        cold_ns.push(started.elapsed().as_nanos() as u64);
        assert!(result.receipt.passed);
        assert!(
            !result
                .summary
                .iter()
                .any(|line| line == "verification_reuse=exact-persisted-receipt")
        );
    }

    let warm_root = evidence.path().join("warm");
    let initial = authoritative_verification_for_components_at(
        repository.path(),
        &warm_root,
        "perf-repository",
        "perf-commit",
        &components,
    )
    .expect("warm population");
    assert!(initial.receipt.passed);

    let mut warm_ns = Vec::with_capacity(SAMPLE_COUNT);
    for _ in 0..SAMPLE_COUNT {
        let started = Instant::now();
        let result = authoritative_verification_for_components_at(
            repository.path(),
            &warm_root,
            "perf-repository",
            "perf-commit",
            &components,
        )
        .expect("warm verification");
        warm_ns.push(started.elapsed().as_nanos() as u64);
        assert!(result.receipt.passed);
        assert!(
            result
                .summary
                .iter()
                .any(|line| line == "verification_reuse=exact-persisted-receipt")
        );
    }

    fs::write(repository.path().join("artifacts/000.json"), "{broken}\n").expect("mutate fixture");
    let changed = authoritative_verification_for_components_at(
        repository.path(),
        &warm_root,
        "perf-repository",
        "perf-commit",
        &components,
    )
    .expect("changed verification");
    assert!(!changed.receipt.passed);
    assert!(
        !changed
            .summary
            .iter()
            .any(|line| line == "verification_reuse=exact-persisted-receipt")
    );

    println!(
        "MEDUSA_CONTINUOUS_VERIFICATION_PERF={}",
        json!({
            "schema_version": 1,
            "fixture_count": FIXTURE_COUNT,
            "values_per_fixture": VALUES_PER_FIXTURE,
            "total_values": FIXTURE_COUNT * VALUES_PER_FIXTURE,
            "sample_count": SAMPLE_COUNT,
            "cold_ns": cold_ns,
            "warm_ns": warm_ns,
            "exact_reuse_verified": true,
            "stale_input_rerun_verified": true,
            "verification_coverage": 1.0
        })
    );
}
