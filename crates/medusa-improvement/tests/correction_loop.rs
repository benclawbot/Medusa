use std::collections::BTreeMap;

use medusa_core::learning_policy::{LearningAdmissionPolicy, LearningPrivacyPolicy};
use medusa_improvement::{
    correction_loop::{CorrectionEpisodeState, CorrectionLoopEngine, CorrectionLoopRequest},
    correction_signals::{ConversationRole, ConversationTurn},
    provenance::{
        PROVENANCE_SCHEMA_VERSION, PrivacyDecision, ProvenanceAuthority, ProvenanceGraph,
        ProvenanceObservation, ProvenanceOutcome, ProvenanceSource, RetentionClass, SourceRange,
    },
    regression_replay::{
        ReplayEnvironment, ReplayObservation, ReplayRunner, ReplayScenario, ReplayScenarioKind,
    },
    solution_selection::SolutionProposal,
};
use time::OffsetDateTime;

fn graph() -> ProvenanceGraph {
    let mut graph = ProvenanceGraph::empty();
    graph
        .insert(ProvenanceObservation {
            id: "user-correction-observation".into(),
            schema_version: PROVENANCE_SCHEMA_VERSION,
            session_id: "session-1".into(),
            root_task_id: "session-1".into(),
            trajectory_id: "trajectory-1".into(),
            attempt_id: "session-1:2".into(),
            parent_id: None,
            worker_id: None,
            tool_name: None,
            source: ProvenanceSource::UserCorrection,
            source_event_id: "event-2".into(),
            source_range: SourceRange {
                start_sequence: 2,
                end_sequence: 2,
            },
            repository: None,
            revision: None,
            scope: "repository".into(),
            observed_at: OffsetDateTime::UNIX_EPOCH,
            ingested_at: OffsetDateTime::UNIX_EPOCH,
            privacy: PrivacyDecision {
                captured: true,
                redacted_fields: vec!["payload_text".into()],
                retention: RetentionClass::Repository,
            },
            authority: ProvenanceAuthority::UserStatement,
            outcome: ProvenanceOutcome::Neutral,
            summary: "user correction recorded".into(),
            typed_payload_digest: "digest".into(),
        })
        .expect("valid observation");
    graph
}

fn request(policy: LearningAdmissionPolicy) -> CorrectionLoopRequest {
    CorrectionLoopRequest {
        session_id: "session-1".into(),
        objective: "complete the repository task".into(),
        turns: vec![
            ConversationTurn {
                id: "turn-1".into(),
                role: ConversationRole::Assistant,
                content: "I claimed the inventory was complete.".into(),
            },
            ConversationTurn {
                id: "turn-2".into(),
                role: ConversationRole::User,
                content: "You missed coverage of the authoritative sources.".into(),
            },
        ],
        provenance: graph(),
        policy,
        repository_fixture: "repository-fixture".into(),
        tool_capabilities: vec!["read".into(), "verify".into()],
        now_unix_ms: 1,
    }
}

#[derive(Default)]
struct ResolvingRunner;

impl ReplayRunner for ResolvingRunner {
    fn run(
        &self,
        scenario: &ReplayScenario,
        candidate: Option<&SolutionProposal>,
        _environment: &ReplayEnvironment,
    ) -> ReplayObservation {
        let is_origin = scenario.kind == ReplayScenarioKind::OriginatingFailure;
        let triggered = candidate.is_some() && is_origin;
        ReplayObservation {
            behavior: if triggered || scenario.kind == ReplayScenarioKind::CriticalSafety {
                scenario.expected_behavior.clone()
            } else if is_origin {
                "baseline failure".into()
            } else {
                "candidate remains inactive".into()
            },
            candidate_triggered: triggered,
            critical_safety_passed: true,
            evidence_links: vec![format!("evidence://{}", scenario.id)],
            metrics: BTreeMap::new(),
        }
    }
}

#[test]
fn correction_runs_through_replay_and_stops_at_shared_review() {
    let repo = tempfile::tempdir().expect("repo");
    let policy = LearningAdmissionPolicy::from_privacy(LearningPrivacyPolicy::private_by_default());
    let engine = CorrectionLoopEngine::default();
    let first = engine
        .run(repo.path(), request(policy.clone()), &ResolvingRunner)
        .expect("correction loop");

    assert_eq!(first.episodes.len(), 1);
    assert_eq!(
        first.episodes[0].state,
        CorrectionEpisodeState::AwaitingReview
    );
    assert!(first.episodes[0].candidate.is_some());
    assert_eq!(
        first.episodes[0].replay.as_ref().expect("replay").decision,
        medusa_improvement::regression_replay::ReplayDecision::Validated
    );

    let authority =
        medusa_improvement::refinement_authority::RefinementAuthorityStore::open(repo.path())
            .expect("authority");
    let snapshot = authority.snapshot().expect("snapshot");
    assert!(snapshot.active.is_empty(), "the loop cannot self-activate");
    assert_eq!(snapshot.records.len(), 1);

    let second = engine
        .run(repo.path(), request(policy), &ResolvingRunner)
        .expect("idempotent correction loop");
    assert_eq!(second.episodes.len(), 1);
    let authority =
        medusa_improvement::refinement_authority::RefinementAuthorityStore::open(repo.path())
            .expect("authority after retry");
    assert_eq!(
        authority
            .snapshot()
            .expect("snapshot after retry")
            .records
            .len(),
        1
    );
}

#[test]
fn harmful_or_nonmatching_replay_is_blocked_without_a_canonical_proposal() {
    let repo = tempfile::tempdir().expect("repo");
    let policy = LearningAdmissionPolicy::from_privacy(LearningPrivacyPolicy::private_by_default());
    #[derive(Default)]
    struct OvertriggeringRunner;
    impl ReplayRunner for OvertriggeringRunner {
        fn run(
            &self,
            scenario: &ReplayScenario,
            candidate: Option<&SolutionProposal>,
            _environment: &ReplayEnvironment,
        ) -> ReplayObservation {
            let candidate_run = candidate.is_some();
            let is_origin = scenario.kind == ReplayScenarioKind::OriginatingFailure;
            ReplayObservation {
                behavior: if (is_origin && candidate_run)
                    || scenario.kind == ReplayScenarioKind::CriticalSafety
                {
                    scenario.expected_behavior.clone()
                } else {
                    "candidate remains inactive".into()
                },
                candidate_triggered: candidate_run,
                critical_safety_passed: true,
                evidence_links: vec![format!("evidence://{}", scenario.id)],
                metrics: BTreeMap::new(),
            }
        }
    }
    let report = CorrectionLoopEngine::default()
        .run(repo.path(), request(policy), &OvertriggeringRunner)
        .expect("correction loop");
    assert_eq!(report.episodes[0].state, CorrectionEpisodeState::Blocked);
    let authority =
        medusa_improvement::refinement_authority::RefinementAuthorityStore::open(repo.path())
            .expect("authority");
    assert!(authority.snapshot().expect("snapshot").records.is_empty());
}

#[test]
fn capture_disabled_is_a_fail_closed_noop() {
    let repo = tempfile::tempdir().expect("repo");
    let policy = LearningAdmissionPolicy::from_privacy(LearningPrivacyPolicy {
        capture_enabled: false,
        user_persistence_enabled: true,
        cross_repository_reuse_enabled: true,
        telemetry_enabled: true,
        automatic_proposals_enabled: true,
    });
    let report = CorrectionLoopEngine::default()
        .run(repo.path(), request(policy), &ResolvingRunner)
        .expect("privacy no-op");
    assert!(report.episodes.is_empty());
    assert!(
        !repo
            .path()
            .join(".medusa/correction-loop/state.json")
            .exists()
    );
}
