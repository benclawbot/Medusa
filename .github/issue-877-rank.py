from pathlib import Path

path = Path('crates/medusa-runtime/src/roadblock_recovery.rs')
text = path.read_text()
old_attempted = '''    let attempted = trajectory
        .repair_ledger
        .iter()
        .flat_map(|failure| failure.repairs.iter())
        .filter(|attempt| {
            attempt.outcome == medusa_session_continuity::VerificationOutcome::Failed
                && !attempt.hypothesis.trim().is_empty()
        })
        .map(|attempt| strategy_signature(&attempt.hypothesis))
        .collect::<BTreeSet<_>>();

    let mut roadblocks = Vec::new();
    for failure in trajectory.repair_ledger.iter().filter(|item| item.unresolved()) {
'''
new_attempted = '''    let mut roadblocks = Vec::new();
    for failure in trajectory.repair_ledger.iter().filter(|item| item.unresolved()) {
        let attempted = failure
            .repairs
            .iter()
            .filter(|attempt| {
                attempt.outcome == medusa_session_continuity::VerificationOutcome::Failed
                    && !attempt.hypothesis.trim().is_empty()
            })
            .map(|attempt| strategy_signature(&attempt.hypothesis))
            .collect::<BTreeSet<_>>();
'''
assert text.count(old_attempted) == 1
text = text.replace(old_attempted, new_attempted)
old_arch = '''            candidate("compatibility-shim", "Preserve the public/architecture contract with an adapter or compatibility shim.", 94, 12, 92),
            candidate("move-change-behind-existing-seam", "Implement behind the repository's existing authority or extension seam instead of changing the public boundary.", 91, 10, 94),
            candidate("decompose-compatible-increments", "Split the change into smaller independently verifiable compatibility-preserving steps.", 84, 6, 97),
'''
new_arch = '''            candidate("compatibility-shim", "Preserve the public/architecture contract with an adapter or compatibility shim.", 98, 6, 99),
            candidate("move-change-behind-existing-seam", "Implement behind the repository's existing authority or extension seam instead of changing the public boundary.", 92, 10, 95),
            candidate("decompose-compatible-increments", "Split the change into smaller independently verifiable compatibility-preserving steps.", 84, 6, 97),
'''
assert text.count(old_arch) == 1
text = text.replace(old_arch, new_arch)
insert_before = '''    #[test]
    fn transition_budget_forces_truthful_escalation() {
'''
extra = '''    #[test]
    fn single_transient_failure_does_not_trigger_recovery() {
        let projected = project(&trajectory(failure("temporary compile hiccup", 1)));
        assert!(projected.roadblocks.is_empty());
        assert!(projected.selected_strategy.is_none());
    }

    #[test]
    fn missing_capability_selects_repository_supported_alternative() {
        let projected = project(&trajectory(failure("required command not found", 1)));
        assert_eq!(projected.roadblocks[0].class, RoadblockClass::MissingCapability);
        assert_eq!(
            projected.roadblocks[0].selected_alternative.as_deref(),
            Some("use-repository-supported-alternative")
        );
    }

    #[test]
    fn unrelated_failed_strategy_does_not_poison_current_roadblock() {
        let mut current = failure("required command not found", 1);
        current.fingerprint = "current".to_owned();
        let mut unrelated = failure("mismatched types", 2);
        unrelated.fingerprint = "unrelated".to_owned();
        unrelated.repairs.push(RepairAttemptCheckpoint {
            id: "unrelated-repair".to_owned(),
            failure_fingerprint: "unrelated".to_owned(),
            changed_files: vec!["crates/a/src/lib.rs".to_owned()],
            outcome: VerificationOutcome::Failed,
            hypothesis: "use-repository-supported-alternative".to_owned(),
            repository_fingerprint: "repo-a".to_owned(),
        });
        let mut state = trajectory(current);
        state.repair_ledger.push(unrelated);
        let projected = project(&state);
        let current_roadblock = projected
            .roadblocks
            .iter()
            .find(|item| item.summary.contains("command not found"))
            .expect("current roadblock");
        assert_eq!(
            current_roadblock.selected_alternative.as_deref(),
            Some("use-repository-supported-alternative")
        );
    }

'''
assert text.count(insert_before) == 1
text = text.replace(insert_before, extra + insert_before)
path.write_text(text)
