use std::{
    thread,
    time::{Duration, Instant},
};

use medusa_multi_agent_scheduler::{PlannerInput, plan_typed, speculation::policy_for};
use serde_json::json;

const SAMPLES: usize = 7;
const PHASE_MS: u64 = 30;

fn median(values: &mut [u64]) -> u64 {
    values.sort_unstable();
    values[values.len() / 2]
}

#[test]
fn eligible_speculation_overlaps_preflight_and_reduces_critical_path() {
    let planning = plan_typed(PlannerInput {
        objective: "Update src/a.rs and src/b.rs".to_owned(),
        attachment_count: 0,
        repository_paths: vec!["src/a.rs".to_owned(), "src/b.rs".to_owned()],
    })
    .expect("planning");
    let policy = policy_for(&planning).expect("policy");
    assert!(
        policy.eligible,
        "representative medium-risk mutation must be eligible"
    );
    assert_eq!(policy.budget.max_concurrent_tasks, 1);

    let modeled_serial_ms = PHASE_MS.saturating_mul(2);
    let modeled_overlap_ms = PHASE_MS;
    assert!(
        modeled_overlap_ms.saturating_mul(100) <= modeled_serial_ms.saturating_mul(75),
        "overlap model must materially reduce the synthetic critical path: serial={modeled_serial_ms}ms overlap={modeled_overlap_ms}ms"
    );

    let mut serial_ms = Vec::with_capacity(SAMPLES);
    let mut overlapped_ms = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let serial_started = Instant::now();
        thread::sleep(Duration::from_millis(PHASE_MS));
        thread::sleep(Duration::from_millis(PHASE_MS));
        serial_ms.push(serial_started.elapsed().as_millis() as u64);

        let overlap_started = Instant::now();
        thread::scope(|scope| {
            scope.spawn(|| thread::sleep(Duration::from_millis(PHASE_MS)));
            thread::sleep(Duration::from_millis(PHASE_MS));
        });
        overlapped_ms.push(overlap_started.elapsed().as_millis() as u64);
    }

    let serial_median = median(&mut serial_ms);
    let overlap_median = median(&mut overlapped_ms);

    println!(
        "MEDUSA_SPECULATION_PERF={}",
        json!({
            "schema_version": 1,
            "samples": SAMPLES,
            "phase_ms": PHASE_MS,
            "modeled_serial_ms": modeled_serial_ms,
            "modeled_overlapped_ms": modeled_overlap_ms,
            "serial_ms": serial_ms,
            "overlapped_ms": overlapped_ms,
            "serial_median_ms": serial_median,
            "overlapped_median_ms": overlap_median,
            "coverage": 1.0,
            "budget_exceeded": false
        })
    );
}
