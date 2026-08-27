use std::{process::Command, sync::mpsc};

use medusa_agent::{AgentSession, record_session_event};
use medusa_context::refinement::{RefinementContent, RefinementLifecycle};
use medusa_core::SessionId;
use medusa_improvement::{
    learning_monitor::LearningMonitorStore, refinement_authority::RefinementAuthorityStore,
};
use medusa_protocol::{Actor, EventPayload};
use medusa_provider::{Message, MessageBlock, Role};
use medusa_runtime::{
    RuntimeEvent,
    learning_retrieval::{self, RuntimeLearningContext},
    learning_review,
    prompt::PromptDraft,
};
use time::OffsetDateTime;

fn repository() -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("repository");
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(repo.path())
            .status()
            .expect("git init")
            .success()
    );
    assert!(
        Command::new("git")
            .args([
                "-c",
                "user.email=tests@example.invalid",
                "-c",
                "user.name=Medusa Tests",
                "commit",
                "--allow-empty",
                "-m",
                "test root",
                "--quiet",
            ])
            .current_dir(repo.path())
            .status()
            .expect("git commit")
            .success()
    );
    let authority = RefinementAuthorityStore::open(repo.path()).expect("authority");
    authority
        .update_privacy(
            medusa_core::learning_policy::LearningPrivacyPolicy {
                capture_enabled: true,
                user_persistence_enabled: true,
                cross_repository_reuse_enabled: true,
                telemetry_enabled: true,
                automatic_proposals_enabled: true,
            },
            0,
        )
        .expect("privacy");
    repo
}

fn build_session(repo: &std::path::Path) -> AgentSession {
    AgentSession {
        id: SessionId::new(),
        objective: "verify authoritative source coverage".to_owned(),
        repo: repo.to_path_buf(),
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
        completed: false,
        turn: 0,
        plan: Vec::new(),
        pending_question: None,
        messages: vec![
            Message {
                role: Role::Assistant,
                content: vec![MessageBlock::Text {
                    text: "I claimed the source inventory was complete.".to_owned(),
                }],
            },
            Message {
                role: Role::User,
                content: vec![MessageBlock::Text {
                    text: "You missed coverage of the authoritative sources.".to_owned(),
                }],
            },
        ],
        events: Vec::new(),
        evidence: Vec::new(),
        tool_artifacts: Vec::new(),
        world_model: None,
        approval_grants: Vec::new(),
        approval_receipts: Vec::new(),
        rollback_receipts: Vec::new(),
        codex_thread_id: None,
    }
}

fn select(repo: &std::path::Path, objective: &str, session_id: &str) -> RuntimeLearningContext {
    let (events, _received) = mpsc::channel::<RuntimeEvent>();
    learning_retrieval::select(
        repo,
        &PromptDraft {
            text: objective.to_owned(),
            ..PromptDraft::default()
        },
        Some(session_id),
        &events,
    )
}

fn proposal_value(record: &medusa_context::refinement::RefinementRecord) -> &str {
    match &record.proposal.as_ref().expect("proposal").after {
        RefinementContent::Memory { value, .. }
        | RefinementContent::RepositoryConvention { value, .. }
        | RefinementContent::PromptGuidance {
            guidance: value, ..
        }
        | RefinementContent::WorkflowMetadata { summary: value, .. }
        | RefinementContent::TeamRoleMetadata {
            guidance: value, ..
        } => value,
    }
}

#[test]
fn correction_to_runtime_outcome_and_rollback_is_production_closed_loop() {
    let repo = repository();
    let mut session = build_session(repo.path());
    let objective = session.objective.clone();

    record_session_event(
        &mut session,
        Actor::User,
        EventPayload::SessionCreated { objective },
    )
    .expect("session creation");
    record_session_event(
        &mut session,
        Actor::User,
        EventPayload::UserPromptReceived {
            text: "You missed coverage of the authoritative sources.".to_owned(),
        },
    )
    .expect("user correction");
    record_session_event(
        &mut session,
        Actor::Coordinator,
        EventPayload::VerificationCompleted {
            passed: true,
            evidence: vec!["authoritative source coverage verified".to_owned()],
        },
    )
    .expect("verification");
    session.completed = true;
    record_session_event(
        &mut session,
        Actor::Coordinator,
        EventPayload::SessionCompleted {
            report_ref: "verified-correction-loop".to_owned(),
        },
    )
    .expect("completion");

    let authority = RefinementAuthorityStore::open(repo.path()).expect("authority");
    let candidate = authority
        .snapshot()
        .expect("candidate snapshot")
        .records
        .into_iter()
        .find(|record| record.lifecycle == RefinementLifecycle::Evaluated)
        .expect("evaluated correction candidate");
    let candidate_id = candidate.proposal_id.clone();
    let matching_objective = proposal_value(&candidate).to_owned();

    let review = learning_review::read(repo.path()).expect("review projection");
    assert_eq!(
        review
            .items
            .iter()
            .find(|item| item.id == candidate_id)
            .map(|item| item.state),
        Some(learning_review::LearningReviewState::Approved)
    );
    let approved = learning_review::transition(
        repo.path(),
        &candidate_id,
        learning_review::LearningReviewState::Approved,
        review.revision,
        "integration-test-user",
    )
    .expect("approval");
    let active = learning_review::transition(
        repo.path(),
        &candidate_id,
        learning_review::LearningReviewState::Active,
        approved.revision,
        "integration-test-user",
    )
    .expect("activation");
    assert_eq!(
        active
            .items
            .iter()
            .find(|item| item.id == candidate_id)
            .map(|item| item.state),
        Some(learning_review::LearningReviewState::Active)
    );

    let mut follow_up = build_session(repo.path());
    follow_up.objective = matching_objective.clone();
    follow_up.messages.clear();
    let follow_up_session_id = follow_up.id.to_string();
    let follow_up_objective = follow_up.objective.clone();
    record_session_event(
        &mut follow_up,
        Actor::User,
        EventPayload::SessionCreated {
            objective: follow_up_objective,
        },
    )
    .expect("follow-up session creation");

    let matching = select(repo.path(), &matching_objective, &follow_up_session_id);
    assert!(
        matching.prompt_context.is_some(),
        "matching task was not selected"
    );
    let nonmatching = select(repo.path(), "write release notes", &follow_up_session_id);
    assert!(
        nonmatching.prompt_context.is_none(),
        "nonmatching task received learned behavior"
    );

    record_session_event(
        &mut follow_up,
        Actor::Coordinator,
        EventPayload::VerificationCompleted {
            passed: true,
            evidence: vec!["repeated source coverage verification".to_owned()],
        },
    )
    .expect("follow-up verification");
    follow_up.completed = true;
    record_session_event(
        &mut follow_up,
        Actor::Coordinator,
        EventPayload::SessionCompleted {
            report_ref: "verified-follow-up".to_owned(),
        },
    )
    .expect("follow-up completion");

    let monitor = LearningMonitorStore::open(repo.path()).expect("monitor");
    let exposure_id = monitor
        .snapshot()
        .artifacts
        .iter()
        .find(|artifact| artifact.artifact_id == candidate_id)
        .and_then(|artifact| artifact.exposures.first())
        .map(|exposure| exposure.id.clone())
        .expect("applied exposure");
    drop(monitor);

    let completed = LearningMonitorStore::open(repo.path())
        .expect("monitor reopen")
        .snapshot();
    let candidate_state = completed
        .artifacts
        .iter()
        .find(|artifact| artifact.artifact_id == candidate_id)
        .expect("candidate monitor state");
    let attributed = candidate_state
        .outcomes
        .iter()
        .find(|outcome| outcome.session_id == follow_up_session_id)
        .expect("attributed follow-up outcome");
    assert_eq!(attributed.exposure_ids, vec![exposure_id]);

    let active_review = learning_review::read(repo.path()).expect("active review");
    let rolled_back = learning_review::transition(
        repo.path(),
        &candidate_id,
        learning_review::LearningReviewState::RolledBack,
        active_review.revision,
        "integration-test-user",
    )
    .expect("rollback");
    assert_eq!(
        rolled_back
            .items
            .iter()
            .find(|item| item.id == candidate_id)
            .map(|item| item.state),
        Some(learning_review::LearningReviewState::RolledBack)
    );
    assert!(
        select(repo.path(), &matching_objective, &follow_up_session_id)
            .prompt_context
            .is_none(),
        "rollback did not restore the baseline behavior"
    );
}
