from pathlib import Path

road = Path('crates/medusa-runtime/src/roadblock_recovery.rs')
text = road.read_text()
anchor = '''    #[test]
    fn transition_budget_forces_truthful_escalation() {
'''
extra = '''    #[test]
    fn repository_conflict_selects_refresh_and_replan() {
        let projected = project(&trajectory(failure("stale repository conflict after head drift", 1)));
        assert_eq!(projected.roadblocks[0].class, RoadblockClass::RepositoryConflict);
        assert_eq!(
            projected.roadblocks[0].selected_alternative.as_deref(),
            Some("refresh-and-replan")
        );
    }

    #[test]
    fn unavailable_dependency_preserves_independent_work_path() {
        let projected = project(&trajectory(failure("service unavailable: dependency offline", 1)));
        assert_eq!(projected.roadblocks[0].class, RoadblockClass::DependencyUnavailable);
        assert!(projected.roadblocks[0]
            .alternatives
            .iter()
            .any(|item| item.strategy == "continue-independent-work"));
    }

    #[test]
    fn platform_capability_can_defer_proof_to_authoritative_ci() {
        let projected = project(&trajectory(failure("unsupported platform tool not installed", 1)));
        assert_eq!(projected.roadblocks[0].class, RoadblockClass::MissingCapability);
        assert!(projected.roadblocks[0]
            .alternatives
            .iter()
            .any(|item| item.strategy == "defer-platform-proof-to-ci"));
    }

'''
assert text.count(anchor) == 1
road.write_text(text.replace(anchor, extra + anchor))

root = Path('crates/medusa-session-continuity/src/root.rs')
text = root.read_text()
fixture_anchor = '''            disproved_hypotheses: vec![DisprovedHypothesisCheckpoint {
                signature: "retry-same-fix".into(),
                repository_fingerprint: "repo-a".into(),
            }],
'''
fixture_repl = '''            roadblocks: vec![RoadblockCheckpoint {
                fingerprint: "roadblock-a".into(),
                class: RoadblockClass::MissingCapability,
                summary: "required platform command unavailable".into(),
                first_generation: 1,
                last_generation: 2,
                repository_fingerprint: "repo-a".into(),
                abandoned_strategy: "run-unavailable-command".into(),
                selected_alternative: Some("defer-platform-proof-to-ci".into()),
                alternatives: vec![AlternativePathCheckpoint {
                    strategy: "defer-platform-proof-to-ci".into(),
                    rationale: "preserve bounded local work".into(),
                    success_probability: 82,
                    blast_radius: 8,
                    verifiability: 96,
                    reversibility: 90,
                    evidence_refs: vec!["journal#4".into()],
                    verification_requirements: vec!["windows-ci".into()],
                    selected: true,
                    rejected_reason: None,
                }],
                source_refs: vec!["journal#4".into()],
                disposition: RoadblockDisposition::AlternativeSelected,
            }],
            strategy_transition_count: 1,
            disproved_hypotheses: vec![DisprovedHypothesisCheckpoint {
                signature: "retry-same-fix".into(),
                repository_fingerprint: "repo-a".into(),
            }],
'''
assert text.count(fixture_anchor) == 1
text = text.replace(fixture_anchor, fixture_repl)
assert_anchor = '''        assert_eq!(second.modified_files, original.modified_files);
        assert_eq!(second.plan_steps, original.plan_steps);
'''
assert_repl = '''        assert_eq!(second.modified_files, original.modified_files);
        assert_eq!(second.plan_steps, original.plan_steps);
        assert_eq!(second.roadblocks, original.roadblocks);
        assert_eq!(second.strategy_transition_count, original.strategy_transition_count);
'''
assert text.count(assert_anchor) == 1
root.write_text(text.replace(assert_anchor, assert_repl))
