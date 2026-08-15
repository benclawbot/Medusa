from pathlib import Path

ledger_path = Path('crates/medusa-runtime/src/repair_ledger.rs')
ledger = ledger_path.read_text()
ledger = ledger.replace(
'''                reconcile_generation(
                    &mut entries,
                    parsed,
                    generation,
                    &changed_files,
                    repository_fingerprint,
                    *passed,
                );''',
'''                reconcile_generation(
                    &mut entries,
                    parsed,
                    generation,
                    &changed_files,
                    repository_fingerprint,
                    &command,
                    *passed,
                );''')
ledger = ledger.replace(
'''    repository_fingerprint: &str,
    passed: bool,
) {''',
'''    repository_fingerprint: &str,
    command: &str,
    passed: bool,
) {''', 1)
ledger = ledger.replace(
'''        if passed {
            existing.transition = RepairLedgerTransition::Resolved;
            existing.last_generation = generation;
        } else if observed_fingerprints.contains(&existing.fingerprint) {''',
'''        if passed && existing.command == command {
            existing.transition = RepairLedgerTransition::Resolved;
            existing.last_generation = generation;
        } else if observed_fingerprints.contains(&existing.fingerprint) {''', 1)
ledger = ledger.replace(
'''        candidate.file == existing.file
            && candidate.test == existing.test''',
'''        candidate.command == existing.command
            && candidate.file == existing.file
            && candidate.test == existing.test''', 1)

marker = '''    #[test]
    fn normalization_deduplicates_location_only_changes() {'''
addition = '''    fn entry(command: &str, file: &str, fingerprint: &str) -> RepairLedgerEntry {
        RepairLedgerEntry {
            fingerprint: fingerprint.to_owned(),
            source: "verification".to_owned(),
            command: command.to_owned(),
            scope: "crates".to_owned(),
            file: Some(file.to_owned()),
            symbol: None,
            test: None,
            diagnostic_class: "compile".to_owned(),
            summary: fingerprint.to_owned(),
            first_generation: 1,
            last_generation: 1,
            occurrence_count: 1,
            changed_details: Vec::new(),
            source_refs: vec!["journal#1".to_owned()],
            root_fingerprint: None,
            cascade: false,
            transition: RepairLedgerTransition::New,
            repairs: Vec::new(),
        }
    }

    #[test]
    fn passing_narrow_check_resolves_only_matching_command() {
        let mut entries = vec![
            entry("cargo check -p alpha", "crates/alpha/src/lib.rs", "alpha"),
            entry("cargo test -p beta", "crates/beta/src/lib.rs", "beta"),
        ];
        reconcile_generation(
            &mut entries,
            Vec::new(),
            2,
            &BTreeSet::new(),
            "repo-a",
            "cargo check -p alpha",
            true,
        );
        assert!(!entries[0].unresolved());
        assert!(entries[1].unresolved());
    }

    #[test]
    fn clusters_cascades_and_retains_new_generation_failures() {
        let evidence = vec![r#"error[E0308]: root mismatch
  --> crates/a/src/lib.rs:12:3
error[E0425]: cascading lookup failure
  --> crates/a/src/lib.rs:18:9"#.to_owned()];
        let mut first = parse_diagnostics(&evidence, "cargo check", "journal#1", 1);
        cluster_common_roots(&mut first);
        assert_eq!(first.len(), 2);
        assert!(!first[0].cascade);
        assert!(first[1].cascade);
        assert_eq!(first[1].root_fingerprint.as_ref(), Some(&first[0].fingerprint));

        let root = first[0].clone();
        let mut entries = first;
        let mut introduced = entry("cargo check", "crates/b/src/lib.rs", "introduced");
        introduced.first_generation = 2;
        introduced.last_generation = 2;
        reconcile_generation(
            &mut entries,
            vec![root, introduced],
            2,
            &BTreeSet::new(),
            "repo-a",
            "cargo check",
            false,
        );
        assert!(entries.iter().any(|entry| entry.fingerprint == "introduced" && entry.first_generation == 2));
        assert!(entries.iter().any(|entry| entry.transition == RepairLedgerTransition::Persisted));
    }

    #[test]
    fn normalization_deduplicates_location_only_changes() {'''
if marker not in ledger:
    raise SystemExit('ledger test marker missing')
ledger = ledger.replace(marker, addition, 1)
ledger_path.write_text(ledger)

root_path = Path('crates/medusa-session-continuity/src/root.rs')
root = root_path.read_text()
marker = '''    #[test]
    fn trajectory_survives_repair_compaction_and_multi_hop_resume() {'''
addition = '''    #[test]
    fn identical_failed_repair_is_blocked_until_strategy_or_repository_changes() {
        let mut value = trajectory();
        value.repair_ledger.push(RepairLedgerEntry {
            fingerprint: "failure:test:resume-mismatch".into(),
            source: "verification".into(),
            command: "cargo test -p medusa-session-continuity".into(),
            scope: "crates".into(),
            file: Some("crates/medusa-session-continuity/src/root.rs".into()),
            symbol: None,
            test: Some("resume".into()),
            diagnostic_class: "test".into(),
            summary: "resume mismatch".into(),
            first_generation: 1,
            last_generation: 1,
            occurrence_count: 1,
            changed_details: Vec::new(),
            source_refs: vec!["journal#7".into()],
            root_fingerprint: None,
            cascade: false,
            transition: RepairLedgerTransition::Persisted,
            repairs: vec![RepairAttemptCheckpoint {
                id: "repair-1".into(),
                failure_fingerprint: "failure:test:resume-mismatch".into(),
                changed_files: vec!["crates/medusa-session-continuity/src/root.rs".into()],
                outcome: VerificationOutcome::Failed,
                hypothesis: "preserve continuity".into(),
                repository_fingerprint: "repo-a".into(),
            }],
        });
        let files = vec!["crates/medusa-session-continuity/src/root.rs".into()];
        assert!(!value.allows_repair_attempt(
            "failure:test:resume-mismatch",
            &files,
            "preserve continuity",
            "repo-a"
        ));
        assert!(value.allows_repair_attempt(
            "failure:test:resume-mismatch",
            &files,
            "different strategy",
            "repo-a"
        ));
        assert!(value.allows_repair_attempt(
            "failure:test:resume-mismatch",
            &files,
            "preserve continuity",
            "repo-b"
        ));
    }

    #[test]
    fn trajectory_survives_repair_compaction_and_multi_hop_resume() {'''
if marker not in root:
    raise SystemExit('continuity test marker missing')
root = root.replace(marker, addition, 1)
root_path.write_text(root)
