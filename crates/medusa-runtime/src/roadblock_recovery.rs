use std::collections::{BTreeMap, BTreeSet};

use medusa_session_continuity::{
    AlternativePathCheckpoint, CodingTrajectoryCheckpoint, RepairLedgerEntry, RoadblockCheckpoint,
    RoadblockClass, RoadblockDisposition,
};
use sha2::{Digest, Sha256};

const MAX_ROADBLOCKS: usize = 64;
const MAX_ALTERNATIVES: usize = 4;
const MAX_TRANSITIONS: u32 = 4;

pub(crate) struct Projection {
    pub roadblocks: Vec<RoadblockCheckpoint>,
    pub selected_strategy: Option<String>,
}

pub(crate) fn project(trajectory: &CodingTrajectoryCheckpoint) -> Projection {
    let repository_fingerprint = trajectory
        .repository
        .as_ref()
        .map(|repo| repo.workspace_fingerprint.as_str())
        .unwrap_or("unknown");
    let prior = trajectory
        .roadblocks
        .iter()
        .map(|item| (item.fingerprint.clone(), item.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut roadblocks = Vec::new();
    for failure in trajectory
        .repair_ledger
        .iter()
        .filter(|item| item.unresolved())
    {
        let attempted = failure
            .repairs
            .iter()
            .filter(|attempt| {
                attempt.outcome == medusa_session_continuity::VerificationOutcome::Failed
                    && !attempt.hypothesis.trim().is_empty()
            })
            .map(|attempt| strategy_signature(&attempt.hypothesis))
            .collect::<BTreeSet<_>>();
        let Some(class) = classify(failure) else {
            continue;
        };
        let fingerprint = roadblock_fingerprint(failure, class, repository_fingerprint);
        let previous = prior.get(&fingerprint);
        let abandoned_strategy = previous
            .and_then(|item| item.selected_alternative.as_ref())
            .cloned()
            .unwrap_or_else(|| current_strategy(failure));
        let mut alternatives = alternatives_for(class, failure, trajectory, &attempted);
        rank(&mut alternatives);

        let can_transition = trajectory.strategy_transition_count < MAX_TRANSITIONS;
        let selected_index = can_transition
            .then(|| {
                alternatives
                    .iter()
                    .position(|item| item.rejected_reason.is_none())
            })
            .flatten();
        let selected_alternative = selected_index.map(|index| alternatives[index].strategy.clone());
        for (index, item) in alternatives.iter_mut().enumerate() {
            item.selected = Some(index) == selected_index;
            if !item.selected && item.rejected_reason.is_none() {
                item.rejected_reason = Some(match selected_alternative.as_deref() {
                    Some(selected) => {
                        format!("lower ranked than selected alternative `{selected}`")
                    }
                    None => "strategy transition budget exhausted".to_owned(),
                });
            }
        }
        let disposition = if selected_alternative.is_some() {
            RoadblockDisposition::AlternativeSelected
        } else {
            RoadblockDisposition::EscalationRequired
        };
        roadblocks.push(RoadblockCheckpoint {
            fingerprint,
            class,
            summary: failure.summary.clone(),
            first_generation: previous
                .map(|item| item.first_generation)
                .unwrap_or(failure.first_generation),
            last_generation: failure.last_generation,
            repository_fingerprint: repository_fingerprint.to_owned(),
            abandoned_strategy,
            selected_alternative,
            alternatives,
            source_refs: failure.source_refs.clone(),
            disposition,
        });
    }

    for previous in prior.values() {
        if roadblocks
            .iter()
            .any(|item| item.fingerprint == previous.fingerprint)
        {
            continue;
        }
        let mut resolved = previous.clone();
        resolved.disposition = RoadblockDisposition::Resolved;
        roadblocks.push(resolved);
    }

    roadblocks.sort_by(|left, right| {
        right
            .unresolved()
            .cmp(&left.unresolved())
            .then_with(|| right.last_generation.cmp(&left.last_generation))
            .then_with(|| left.fingerprint.cmp(&right.fingerprint))
    });
    roadblocks.truncate(MAX_ROADBLOCKS);
    let selected_strategy = roadblocks
        .iter()
        .find(|item| item.unresolved())
        .and_then(|item| item.selected_alternative.clone());
    Projection {
        roadblocks,
        selected_strategy,
    }
}

fn classify(failure: &RepairLedgerEntry) -> Option<RoadblockClass> {
    let text = format!("{} {}", failure.diagnostic_class, failure.summary).to_ascii_lowercase();
    if contains_any(
        &text,
        &[
            "command not found",
            "not installed",
            "unsupported platform",
            "unavailable tool",
            "missing capability",
        ],
    ) {
        return Some(RoadblockClass::MissingCapability);
    }
    if contains_any(
        &text,
        &[
            "connection refused",
            "service unavailable",
            "dependency unavailable",
            "offline",
            "dns",
        ],
    ) {
        return Some(RoadblockClass::DependencyUnavailable);
    }
    if contains_any(
        &text,
        &[
            "breaking change",
            "public api",
            "architecture",
            "compatibility",
            "forbidden dependency",
        ],
    ) {
        return Some(RoadblockClass::ArchitectureCompatibility);
    }
    if contains_any(
        &text,
        &[
            "permission denied",
            "not permitted",
            "forbidden",
            "policy",
            "approval required",
        ],
    ) {
        return Some(RoadblockClass::PermissionPolicy);
    }
    if contains_any(
        &text,
        &[
            "stale",
            "conflict",
            "non-fast-forward",
            "repository drift",
            "lock file changed",
        ],
    ) {
        return Some(RoadblockClass::RepositoryConflict);
    }
    if contains_any(
        &text,
        &[
            "out of memory",
            "budget exceeded",
            "resource exhausted",
            "quota exceeded",
        ],
    ) {
        return Some(RoadblockClass::ResourceExhaustion);
    }
    if contains_any(
        &text,
        &[
            "assumption",
            "hypothesis",
            "does not exist",
            "no method named",
            "unresolved import",
        ],
    ) {
        return Some(RoadblockClass::DisprovedHypothesis);
    }
    if contains_any(
        &text,
        &[
            "structurally wrong",
            "invariant violation",
            "design cannot satisfy",
            "structural verification",
        ],
    ) {
        return Some(RoadblockClass::StructuralVerification);
    }
    if failure.occurrence_count >= 2
        || failure
            .repairs
            .iter()
            .filter(|attempt| {
                attempt.outcome == medusa_session_continuity::VerificationOutcome::Failed
            })
            .count()
            >= 1
    {
        return Some(RoadblockClass::DeterministicFailure);
    }
    None
}

fn alternatives_for(
    class: RoadblockClass,
    failure: &RepairLedgerEntry,
    trajectory: &CodingTrajectoryCheckpoint,
    attempted: &BTreeSet<String>,
) -> Vec<AlternativePathCheckpoint> {
    let mut candidates = match class {
        RoadblockClass::MissingCapability => vec![
            candidate(
                "use-repository-supported-alternative",
                "Discover and use the repository's supported equivalent tool or command.",
                92,
                10,
                94,
            ),
            candidate(
                "defer-platform-proof-to-ci",
                "Continue independent work and bind the unavailable platform proof to authoritative CI.",
                82,
                8,
                96,
            ),
            candidate(
                "deterministic-local-fixture",
                "Replace unavailable live/tool dependency with a deterministic local fixture when authoritative.",
                78,
                14,
                90,
            ),
        ],
        RoadblockClass::DependencyUnavailable => vec![
            candidate(
                "deterministic-local-fixture",
                "Use a local deterministic fixture or cached evidence instead of the unavailable service.",
                88,
                10,
                92,
            ),
            candidate(
                "continue-independent-work",
                "Complete independent changes and preserve the external dependency as an explicit blocker.",
                76,
                5,
                98,
            ),
            candidate(
                "allowed-provider-or-service-fallback",
                "Use an already-authorized fallback route without expanding capability scope.",
                80,
                12,
                88,
            ),
        ],
        RoadblockClass::PermissionPolicy => vec![
            candidate(
                "narrow-scope-implementation",
                "Choose an implementation that remains inside current approval, write, network, and capability boundaries.",
                90,
                8,
                96,
            ),
            candidate(
                "complete-independent-work-and-escalate",
                "Finish independent work and preserve the exact permission boundary as a resumable blocker.",
                70,
                2,
                100,
            ),
        ],
        RoadblockClass::ArchitectureCompatibility => vec![
            candidate(
                "compatibility-shim",
                "Preserve the public/architecture contract with an adapter or compatibility shim.",
                98,
                6,
                99,
            ),
            candidate(
                "move-change-behind-existing-seam",
                "Implement behind the repository's existing authority or extension seam instead of changing the public boundary.",
                92,
                10,
                95,
            ),
            candidate(
                "decompose-compatible-increments",
                "Split the change into smaller independently verifiable compatibility-preserving steps.",
                84,
                6,
                97,
            ),
        ],
        RoadblockClass::RepositoryConflict => vec![
            candidate(
                "refresh-and-replan",
                "Refresh repository state, invalidate stale evidence, and regenerate the mutation plan.",
                96,
                4,
                99,
            ),
            candidate(
                "isolate-smaller-mutation",
                "Use a smaller isolated edit/integration strategy against freshly read source.",
                86,
                8,
                96,
            ),
        ],
        RoadblockClass::DisprovedHypothesis => vec![
            candidate(
                "evidence-backed-alternative-api",
                "Re-read exact source/API evidence and choose an implementation supported by the observed contract.",
                95,
                9,
                95,
            ),
            candidate(
                "different-layer-implementation",
                "Satisfy the objective through a different module or layer with less unsupported assumption.",
                84,
                13,
                90,
            ),
            candidate(
                "decompose-and-probe",
                "Run a narrow deterministic probe, then implement only the proven branch.",
                87,
                7,
                96,
            ),
        ],
        RoadblockClass::StructuralVerification => vec![
            candidate(
                "redesign-behind-authoritative-seam",
                "Replace the structurally invalid design while preserving the authoritative verification contract.",
                92,
                18,
                88,
            ),
            candidate(
                "decompose-compatible-increments",
                "Reduce blast radius and verify each architectural increment independently.",
                83,
                8,
                95,
            ),
        ],
        RoadblockClass::ResourceExhaustion => vec![
            candidate(
                "narrow-work-and-verification",
                "Reduce context, mutation, and verification scope while preserving required final gates.",
                88,
                5,
                96,
            ),
            candidate(
                "defer-heavy-proof-to-authoritative-ci",
                "Continue bounded local work and move only the heavy proof to authoritative CI.",
                80,
                4,
                94,
            ),
        ],
        RoadblockClass::DeterministicFailure => vec![
            candidate(
                "refresh-evidence-and-change-strategy",
                "Re-read exact failure/source evidence and choose a materially different repair strategy.",
                94,
                7,
                97,
            ),
            candidate(
                "decompose-independent-failures",
                "Separate independent failures and repair the smallest authoritative root first without repeating the failed edit.",
                87,
                6,
                96,
            ),
            candidate(
                "alternate-verification-route",
                "Use the narrowest equivalent authoritative check to test a different hypothesis before broad rerun.",
                80,
                4,
                94,
            ),
        ],
    };

    for item in &mut candidates {
        let signature = strategy_signature(&item.strategy);
        if attempted.contains(&signature) {
            item.rejected_reason =
                Some("equivalent strategy was already attempted for a prior roadblock".to_owned());
        }
        if widens_authority(&item.strategy) {
            item.rejected_reason = Some("strategy would widen current authority".to_owned());
        }
        item.evidence_refs = failure.source_refs.iter().take(4).cloned().collect();
        item.verification_requirements = trajectory
            .verification_requirements
            .iter()
            .take(4)
            .cloned()
            .collect();
    }
    candidates.truncate(MAX_ALTERNATIVES);
    candidates
}

fn rank(items: &mut [AlternativePathCheckpoint]) {
    items.sort_by(|left, right| {
        score(right)
            .cmp(&score(left))
            .then_with(|| left.strategy.cmp(&right.strategy))
    });
}

fn score(item: &AlternativePathCheckpoint) -> i32 {
    i32::from(item.success_probability)
        + i32::from(item.verifiability)
        + i32::from(item.reversibility)
        - i32::from(item.blast_radius)
}

fn candidate(
    strategy: &str,
    rationale: &str,
    success_probability: u8,
    blast_radius: u8,
    verifiability: u8,
) -> AlternativePathCheckpoint {
    AlternativePathCheckpoint {
        strategy: strategy.to_owned(),
        rationale: rationale.to_owned(),
        success_probability,
        blast_radius,
        verifiability,
        reversibility: 90,
        evidence_refs: Vec::new(),
        verification_requirements: Vec::new(),
        selected: false,
        rejected_reason: None,
    }
}

fn current_strategy(failure: &RepairLedgerEntry) -> String {
    failure
        .repairs
        .last()
        .map(|repair| repair.hypothesis.clone())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("repair:{}", failure.diagnostic_class))
}

fn roadblock_fingerprint(
    failure: &RepairLedgerEntry,
    class: RoadblockClass,
    repository_fingerprint: &str,
) -> String {
    digest(
        format!(
            "{:?}|{}|{}|{}",
            class, failure.fingerprint, failure.command, repository_fingerprint
        )
        .as_bytes(),
    )
}

fn strategy_signature(strategy: &str) -> String {
    digest(
        strategy
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
            .as_bytes(),
    )
}

fn widens_authority(strategy: &str) -> bool {
    let value = strategy.to_ascii_lowercase();
    contains_any(
        &value,
        &[
            "disable policy",
            "bypass approval",
            "expand write",
            "unrestricted network",
        ],
    )
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn digest(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

#[cfg(test)]
mod tests {
    use medusa_session_continuity::{
        RepairAttemptCheckpoint, RepairLedgerTransition, RepositoryCheckpoint, VerificationOutcome,
    };

    use super::*;

    fn failure(summary: &str, occurrences: u32) -> RepairLedgerEntry {
        RepairLedgerEntry {
            fingerprint: "failure-a".to_owned(),
            source: "verification".to_owned(),
            command: "cargo check".to_owned(),
            scope: "crates/a".to_owned(),
            file: Some("crates/a/src/lib.rs".to_owned()),
            symbol: None,
            test: None,
            diagnostic_class: "compile".to_owned(),
            summary: summary.to_owned(),
            first_generation: 1,
            last_generation: occurrences.into(),
            occurrence_count: occurrences,
            changed_details: Vec::new(),
            source_refs: vec![".medusa/sessions/s/journal.jsonl#3".to_owned()],
            root_fingerprint: None,
            cascade: false,
            transition: RepairLedgerTransition::Persisted,
            repairs: Vec::new(),
        }
    }

    fn trajectory(entry: RepairLedgerEntry) -> CodingTrajectoryCheckpoint {
        CodingTrajectoryCheckpoint {
            repair_ledger: vec![entry],
            verification_requirements: vec!["cargo check".to_owned(), "cargo test".to_owned()],
            repository: Some(RepositoryCheckpoint {
                head: Some("abc".to_owned()),
                workspace_fingerprint: "repo-a".to_owned(),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn repeated_deterministic_failure_selects_materially_different_strategy() {
        let projected = project(&trajectory(failure("mismatched types", 2)));
        assert_eq!(projected.roadblocks.len(), 1);
        let roadblock = &projected.roadblocks[0];
        assert_eq!(roadblock.class, RoadblockClass::DeterministicFailure);
        assert!(roadblock.alternatives.len() >= 2);
        assert!(roadblock.selected_alternative.is_some());
        assert!(roadblock.alternatives.iter().any(|item| item.selected));
    }

    #[test]
    fn architecture_failure_prefers_compatibility_preserving_path() {
        let projected = project(&trajectory(failure(
            "public API compatibility policy rejects breaking change",
            1,
        )));
        assert_eq!(
            projected.roadblocks[0].class,
            RoadblockClass::ArchitectureCompatibility
        );
        assert_eq!(
            projected.roadblocks[0].selected_alternative.as_deref(),
            Some("compatibility-shim")
        );
    }

    #[test]
    fn prior_equivalent_strategy_is_not_selected_again() {
        let mut state = trajectory(failure("mismatched types", 2));
        let first = project(&state);
        state.roadblocks = first.roadblocks;
        state.strategy_transition_count = 1;
        state.repair_ledger[0]
            .repairs
            .push(RepairAttemptCheckpoint {
                id: "repair-2".to_owned(),
                failure_fingerprint: "failure-a".to_owned(),
                changed_files: vec!["crates/a/src/lib.rs".to_owned()],
                outcome: VerificationOutcome::Failed,
                hypothesis: state.roadblocks[0]
                    .selected_alternative
                    .clone()
                    .expect("strategy"),
                repository_fingerprint: "repo-a".to_owned(),
            });
        let second = project(&state);
        assert_ne!(
            second.roadblocks[0].selected_alternative,
            state.roadblocks[0].selected_alternative
        );
    }

    #[test]
    fn single_transient_failure_does_not_trigger_recovery() {
        let projected = project(&trajectory(failure("temporary compile hiccup", 1)));
        assert!(projected.roadblocks.is_empty());
        assert!(projected.selected_strategy.is_none());
    }

    #[test]
    fn missing_capability_selects_repository_supported_alternative() {
        let projected = project(&trajectory(failure("required command not found", 1)));
        assert_eq!(
            projected.roadblocks[0].class,
            RoadblockClass::MissingCapability
        );
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

    #[test]
    fn repository_conflict_selects_refresh_and_replan() {
        let projected = project(&trajectory(failure(
            "stale repository conflict after head drift",
            1,
        )));
        assert_eq!(
            projected.roadblocks[0].class,
            RoadblockClass::RepositoryConflict
        );
        assert_eq!(
            projected.roadblocks[0].selected_alternative.as_deref(),
            Some("refresh-and-replan")
        );
    }

    #[test]
    fn unavailable_dependency_preserves_independent_work_path() {
        let projected = project(&trajectory(failure(
            "service unavailable: dependency offline",
            1,
        )));
        assert_eq!(
            projected.roadblocks[0].class,
            RoadblockClass::DependencyUnavailable
        );
        assert!(
            projected.roadblocks[0]
                .alternatives
                .iter()
                .any(|item| item.strategy == "continue-independent-work")
        );
    }

    #[test]
    fn platform_capability_can_defer_proof_to_authoritative_ci() {
        let projected = project(&trajectory(failure(
            "unsupported platform tool not installed",
            1,
        )));
        assert_eq!(
            projected.roadblocks[0].class,
            RoadblockClass::MissingCapability
        );
        assert!(
            projected.roadblocks[0]
                .alternatives
                .iter()
                .any(|item| item.strategy == "defer-platform-proof-to-ci")
        );
    }

    #[test]
    fn transition_budget_forces_truthful_escalation() {
        let mut state = trajectory(failure("mismatched types", 3));
        state.strategy_transition_count = MAX_TRANSITIONS;
        let projected = project(&state);
        assert_eq!(
            projected.roadblocks[0].disposition,
            RoadblockDisposition::EscalationRequired
        );
        assert!(projected.roadblocks[0].selected_alternative.is_none());
    }
}
