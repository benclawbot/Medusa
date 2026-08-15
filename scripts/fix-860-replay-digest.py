#!/usr/bin/env python3
from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one occurrence, found {count}")
    p.write_text(text.replace(old, new, 1))


replace_once(
    "crates/medusa-improvement/src/lib.rs",
    "#[allow(clippy::expect_used)]\npub mod regression_replay;",
    "pub mod regression_replay;",
)

old_digest = '''fn digest_bundle(bundle: &ReplayBundle) -> String {
    let mut clone = bundle.clone();
    clone.digest.clear();
    let bytes = serde_json::to_vec(&clone).expect("serializable replay bundle");
    format!("{:x}", Sha256::digest(bytes))
}
'''
new_digest = '''/// Stable, infallible canonical encoding for replay-bundle digests.
///
/// Every digest-bound value is length-prefixed or fixed-width and the digest field itself is
/// deliberately excluded. The version tag prevents a future encoding revision from silently
/// colliding with this contract.
fn digest_bundle(bundle: &ReplayBundle) -> String {
    fn bytes(hasher: &mut Sha256, value: &[u8]) {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value);
    }

    fn text(hasher: &mut Sha256, value: &str) {
        bytes(hasher, value.as_bytes());
    }

    fn strings(hasher: &mut Sha256, values: &[String]) {
        hasher.update((values.len() as u64).to_be_bytes());
        for value in values {
            text(hasher, value);
        }
    }

    fn scenario_kind(kind: ReplayScenarioKind) -> u8 {
        match kind {
            ReplayScenarioKind::OriginatingFailure => 0,
            ReplayScenarioKind::IntendedContext => 1,
            ReplayScenarioKind::NonApplicableContext => 2,
            ReplayScenarioKind::CriticalSafety => 3,
        }
    }

    let mut hasher = Sha256::new();
    hasher.update(b"medusa-replay-bundle-digest-v1\\0");
    text(&mut hasher, &bundle.id);
    text(&mut hasher, &bundle.lesson_id);
    strings(&mut hasher, &bundle.source_signal_ids);
    text(&mut hasher, &bundle.repository_fixture);
    strings(&mut hasher, &bundle.tool_capabilities);
    strings(&mut hasher, &bundle.artifact_expectations);
    text(&mut hasher, &bundle.failure_evidence);
    hasher.update((bundle.scenarios.len() as u64).to_be_bytes());
    for scenario in &bundle.scenarios {
        text(&mut hasher, &scenario.id);
        hasher.update([scenario_kind(scenario.kind)]);
        text(&mut hasher, &scenario.input);
        text(&mut hasher, &scenario.expected_behavior);
        hasher.update([u8::from(scenario.candidate_should_trigger)]);
    }
    text(&mut hasher, &bundle.environment.model);
    text(&mut hasher, &bundle.environment.provider);
    text(&mut hasher, &bundle.environment.runtime_version);
    hasher.update(bundle.environment.seed.to_be_bytes());
    hasher.update(bundle.environment.temperature_milli.to_be_bytes());
    hasher.update([u8::from(bundle.environment.network_enabled)]);
    hasher.update(bundle.environment.clock_epoch_ms.to_be_bytes());
    hasher.update([u8::from(bundle.redacted)]);
    format!("{:x}", hasher.finalize())
}
'''
replace_once(
    "crates/medusa-improvement/src/regression_replay.rs", old_digest, new_digest
)
replace_once(
    "crates/medusa-improvement/src/regression_replay.rs",
    "#[cfg(test)]\nmod tests {\n",
    "#[cfg(test)]\n#[allow(clippy::unwrap_used, clippy::expect_used)]\nmod tests {\n",
)

marker = '''    #[test]
    fn validation_requires_baseline_reproduction_resolution_and_no_overtriggering() {
'''
test = '''    #[test]
    fn digest_binds_replay_inputs_and_ignores_existing_digest() {
        let lesson = lesson();
        let proposal = SolutionSelector.propose(&lesson);
        let bundle = ReplayBundleBuilder.build(
            &lesson,
            &proposal,
            "repo",
            vec!["read".to_owned(), "write".to_owned()],
        );
        let original = bundle.digest.clone();

        let mut stale_digest = bundle.clone();
        stale_digest.digest = "stale-or-unverified".to_owned();
        assert_eq!(digest_bundle(&stale_digest), original);

        let mutations: Vec<Box<dyn Fn(&mut ReplayBundle)>> = vec![
            Box::new(|value| value.id.push_str("-changed")),
            Box::new(|value| value.lesson_id.push_str("-changed")),
            Box::new(|value| value.source_signal_ids.push("extra-signal".to_owned())),
            Box::new(|value| value.repository_fixture.push_str("-changed")),
            Box::new(|value| value.tool_capabilities.push("shell".to_owned())),
            Box::new(|value| value.artifact_expectations.push("extra-artifact".to_owned())),
            Box::new(|value| value.failure_evidence.push_str("-changed")),
            Box::new(|value| value.scenarios[0].input.push_str("-changed")),
            Box::new(|value| value.environment.model.push_str("-changed")),
            Box::new(|value| value.environment.seed = value.environment.seed.wrapping_add(1)),
            Box::new(|value| value.redacted = !value.redacted),
        ];
        for mutate in mutations {
            let mut changed = bundle.clone();
            mutate(&mut changed);
            assert_ne!(digest_bundle(&changed), original);
        }
    }

'''
replace_once(
    "crates/medusa-improvement/src/regression_replay.rs", marker, test + marker
)

replace_once(
    ".github/workflows/ci.yml",
    "      - name: Production panic audit\n        id: production_panic_audit\n        shell: bash\n        run: |\n          set -o pipefail\n          cargo clippy --workspace --all-features --locked --lib --bins --examples -- \\\n",
    "      - name: Production panic audit\n        id: production_panic_audit\n        shell: bash\n        run: |\n          set -o pipefail\n          python3 scripts/test-production-panic-exemptions.py\n          python3 scripts/check-production-panic-exemptions.py\n          cargo clippy --workspace --all-features --locked --lib --bins --examples -- \\\n",
)
