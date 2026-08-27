use std::{env, fs, path::Path, process::Command, sync::mpsc};

use medusa_agent::{AgentSession, record_session_event};
use medusa_config::Config;
use medusa_context::refinement::{RefinementContent, RefinementLifecycle};
use medusa_core::SessionId;
use medusa_improvement::{
    learning_monitor::{ExposureState, LearningMonitorStore, OutcomeStatus},
    refinement_authority::RefinementAuthorityStore,
};
use medusa_protocol::{Actor, EventPayload};
use medusa_provider::{
    ConfiguredProvider, Message, MessageBlock, ModelProvider, ModelRequest, ResponseBlock, Role,
};
use medusa_runtime::{
    RuntimeEvent,
    learning_retrieval::{self, RuntimeLearningContext},
    learning_review,
    prompt::PromptDraft,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

const CORRECTION: &str =
    "You missed beta when checking the authoritative source inventory; verify both alpha and beta.";

#[derive(Serialize)]
struct RedactedTextEvidence {
    sha256: String,
    character_count: usize,
}

#[derive(Serialize)]
struct AuthorityEvidence {
    proposal_id_sha256: String,
    proposal_version: u64,
    baseline_active_sha256: String,
    activated_sha256: String,
    restored_active_sha256: String,
}

#[derive(Serialize)]
struct LoopEvidence {
    correction_candidate_evaluated: bool,
    approved_and_activated: bool,
    matching_context_applied: bool,
    repeated_verified_outcomes: usize,
    nonmatching_task_unaffected: bool,
    rollback_restored_exact_baseline: bool,
}

#[derive(Serialize)]
struct PrivacyEvidence {
    credential_persisted: bool,
    raw_transcript_in_report: bool,
    capture_disabled_blocks_new_candidate: bool,
}

#[derive(Serialize)]
struct LiveSelfImprovementEvidence {
    schema_version: u32,
    commit: String,
    platform: String,
    recorded_at_unix_ms: i64,
    provider: &'static str,
    model: String,
    live_request_count: usize,
    initial_assistant: RedactedTextEvidence,
    correction: RedactedTextEvidence,
    follow_up_assistants: Vec<RedactedTextEvidence>,
    authority: AuthorityEvidence,
    production_loop: LoopEvidence,
    privacy: PrivacyEvidence,
}

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
                "user.email=live-acceptance@example.invalid",
                "-c",
                "user.name=Medusa Live Acceptance",
                "commit",
                "--allow-empty",
                "-m",
                "live acceptance root",
                "--quiet",
            ])
            .current_dir(repo.path())
            .status()
            .expect("git commit")
            .success()
    );
    RefinementAuthorityStore::open(repo.path())
        .expect("authority")
        .update_privacy(enabled_privacy(), 0)
        .expect("enable learning privacy controls");
    repo
}

fn enabled_privacy() -> medusa_core::learning_policy::LearningPrivacyPolicy {
    medusa_core::learning_policy::LearningPrivacyPolicy {
        capture_enabled: true,
        user_persistence_enabled: true,
        cross_repository_reuse_enabled: true,
        telemetry_enabled: true,
        automatic_proposals_enabled: true,
    }
}

fn disabled_privacy() -> medusa_core::learning_policy::LearningPrivacyPolicy {
    medusa_core::learning_policy::LearningPrivacyPolicy {
        capture_enabled: false,
        user_persistence_enabled: false,
        cross_repository_reuse_enabled: false,
        telemetry_enabled: false,
        automatic_proposals_enabled: false,
    }
}

fn session(repo: &Path, objective: &str, messages: Vec<Message>) -> AgentSession {
    AgentSession {
        id: SessionId::new(),
        objective: objective.to_owned(),
        repo: repo.to_path_buf(),
        created_at: OffsetDateTime::now_utc(),
        updated_at: OffsetDateTime::now_utc(),
        completed: false,
        turn: 0,
        plan: Vec::new(),
        pending_question: None,
        messages,
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

fn required_model(value: Option<&str>) -> Result<String, &'static str> {
    let model = value.map(str::trim).filter(|model| !model.is_empty());
    model
        .map(ToOwned::to_owned)
        .ok_or("MEDUSA_MODEL is required and must not be empty")
}

fn provider(api_key: String, model: &str) -> impl ModelProvider {
    let mut config = Config::default();
    config.model.provider = "minimax".to_owned();
    config.model.name = model.to_owned();
    config.model.tool_calling = false;
    ConfiguredProvider::from_config_with_api_key(&config, Some(api_key))
        .expect("configured MiniMax provider")
}

fn live_completion(provider: &impl ModelProvider, system: &str, user: &str) -> String {
    let response = provider
        .complete(&ModelRequest {
            system: system.to_owned(),
            messages: vec![Message {
                role: Role::User,
                content: vec![MessageBlock::Text {
                    text: user.to_owned(),
                }],
            }],
            tools: Vec::new(),
            // MiniMax M2.7 spends part of the bounded completion budget on hidden reasoning;
            // leave enough room for the visible self-improvement assessment on later turns.
            max_tokens: 1024,
            temperature_milli: 0,
        })
        .expect("live MiniMax completion");
    let text = response
        .blocks
        .into_iter()
        .filter_map(|block| match block {
            ResponseBlock::Text { text } => Some(text),
            ResponseBlock::ToolUse { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.trim().chars().count() >= 24,
        "live response was unexpectedly short"
    );
    text
}

fn select(repo: &Path, objective: &str, session_id: &str) -> RuntimeLearningContext {
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

fn record_verified_completion(session: &mut AgentSession, report_ref: &str) {
    record_session_event(
        session,
        Actor::Coordinator,
        EventPayload::VerificationCompleted {
            passed: true,
            evidence: vec!["authoritative live acceptance verification passed".to_owned()],
        },
    )
    .expect("verification");
    session.completed = true;
    record_session_event(
        session,
        Actor::Coordinator,
        EventPayload::SessionCompleted {
            report_ref: report_ref.to_owned(),
        },
    )
    .expect("completion");
}

fn run_live_follow_up(
    repo: &Path,
    provider: &impl ModelProvider,
    objective: &str,
    index: usize,
) -> (String, String) {
    let mut follow_up = session(repo, objective, Vec::new());
    let session_id = follow_up.id.to_string();
    record_session_event(
        &mut follow_up,
        Actor::User,
        EventPayload::SessionCreated {
            objective: objective.to_owned(),
        },
    )
    .expect("follow-up session creation");

    let matching = select(repo, objective, &session_id);
    let prompt_context = matching
        .prompt_context
        .expect("matching task did not receive active learned behavior");
    let assistant = live_completion(
        provider,
        &format!(
            "{prompt_context}\n\nApply the learned instruction. Return a concise source-coverage assessment."
        ),
        "Check the authoritative inventory now and state whether both required sources were considered.",
    );
    follow_up.messages.push(Message {
        role: Role::Assistant,
        content: vec![MessageBlock::Text {
            text: assistant.clone(),
        }],
    });
    record_verified_completion(&mut follow_up, &format!("live-follow-up-{index}"));
    (session_id, assistant)
}

fn digest_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn digest_serialized<T: Serialize>(value: &T) -> String {
    digest_bytes(&serde_json::to_vec(value).expect("serialize digest input"))
}

fn redacted_text(text: &str) -> RedactedTextEvidence {
    RedactedTextEvidence {
        sha256: digest_bytes(text.as_bytes()),
        character_count: text.chars().count(),
    }
}

fn tree_contains(root: &Path, needle: &[u8]) -> bool {
    fs::read_dir(root)
        .expect("read acceptance repository")
        .filter_map(Result::ok)
        .any(|entry| {
            let path = entry.path();
            if path.is_dir() {
                tree_contains(&path, needle)
            } else {
                fs::read(path)
                    .ok()
                    .is_some_and(|bytes| bytes.windows(needle.len()).any(|window| window == needle))
            }
        })
}

#[test]
fn non_default_live_model_selection_is_preserved() {
    assert_eq!(
        required_model(Some("MiniMax-M2.7")).expect("non-default model"),
        "MiniMax-M2.7"
    );
    assert_eq!(
        required_model(Some("  MiniMax-M3  ")).expect("trimmed model"),
        "MiniMax-M3"
    );
    assert!(required_model(None).is_err());
    assert!(required_model(Some("   ")).is_err());
}

#[test]
#[ignore = "requires the protected MINIMAX_API_KEY live-test secret"]
fn live_correction_repeats_verified_outcomes_and_rolls_back_exactly() {
    let report_path = env::var_os("MEDUSA_LIVE_SELF_IMPROVEMENT_REPORT")
        .map(std::path::PathBuf::from)
        .expect("MEDUSA_LIVE_SELF_IMPROVEMENT_REPORT is required");
    let api_key = env::var("MINIMAX_API_KEY").expect("MINIMAX_API_KEY is required");
    assert!(api_key.len() >= 20, "live credential is unexpectedly short");
    let model_env = env::var("MEDUSA_MODEL").ok();
    let model = required_model(model_env.as_deref()).expect("valid MEDUSA_MODEL");
    let provider = provider(api_key.clone(), &model);
    let repo = repository();

    let initial_assistant = live_completion(
        &provider,
        "You are participating in a bounded Medusa self-improvement acceptance test. Do not use tools.",
        "Make one sentence claiming the authoritative source inventory check is complete and that alpha was considered.",
    );
    let objective = "verify alpha and beta in the authoritative source inventory";
    let mut corrected = session(
        repo.path(),
        objective,
        vec![
            Message {
                role: Role::Assistant,
                content: vec![MessageBlock::Text {
                    text: initial_assistant.clone(),
                }],
            },
            Message {
                role: Role::User,
                content: vec![MessageBlock::Text {
                    text: CORRECTION.to_owned(),
                }],
            },
        ],
    );
    record_session_event(
        &mut corrected,
        Actor::User,
        EventPayload::SessionCreated {
            objective: objective.to_owned(),
        },
    )
    .expect("corrected session creation");
    record_session_event(
        &mut corrected,
        Actor::User,
        EventPayload::UserPromptReceived {
            text: CORRECTION.to_owned(),
        },
    )
    .expect("real correction");
    record_verified_completion(&mut corrected, "live-correction");

    let authority = RefinementAuthorityStore::open(repo.path()).expect("authority");
    let baseline = authority.snapshot().expect("baseline snapshot");
    let candidate = baseline
        .records
        .iter()
        .find(|record| record.lifecycle == RefinementLifecycle::Evaluated)
        .cloned()
        .expect("evaluated correction candidate");
    let candidate_id = candidate.proposal_id.clone();
    let matching_objective = proposal_value(&candidate).to_owned();
    let baseline_active_sha256 = digest_serialized(&baseline.active);
    drop(authority);

    let review = learning_review::read(repo.path()).expect("review projection");
    let approved = learning_review::transition(
        repo.path(),
        &candidate_id,
        learning_review::LearningReviewState::Approved,
        review.revision,
        "live-acceptance-user",
    )
    .expect("approval");
    let active = learning_review::transition(
        repo.path(),
        &candidate_id,
        learning_review::LearningReviewState::Active,
        approved.revision,
        "live-acceptance-user",
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
    let activated = RefinementAuthorityStore::open(repo.path())
        .expect("active authority")
        .snapshot()
        .expect("active snapshot");
    assert_eq!(activated.active.len(), 1);
    let activated_sha256 = digest_serialized(&activated.active);

    let first = run_live_follow_up(repo.path(), &provider, &matching_objective, 1);
    let second = run_live_follow_up(repo.path(), &provider, &matching_objective, 2);
    let nonmatching_unaffected = select(
        repo.path(),
        "write concise release notes",
        "live-nonmatching-session",
    )
    .prompt_context
    .is_none();
    assert!(nonmatching_unaffected, "nonmatching task was modified");

    let monitor = LearningMonitorStore::open(repo.path())
        .expect("monitor")
        .snapshot();
    let candidate_monitor = monitor
        .artifacts
        .iter()
        .find(|artifact| artifact.artifact_id == candidate_id)
        .expect("candidate monitor state");
    let follow_up_session_ids = [&first.0, &second.0];
    let repeated_verified_outcomes = candidate_monitor
        .outcomes
        .iter()
        .filter(|outcome| {
            follow_up_session_ids.contains(&&outcome.session_id)
                && outcome.status == OutcomeStatus::Positive
                && outcome.verification_passed == Some(true)
                && outcome.exposure_ids.len() == 1
        })
        .count();
    assert_eq!(repeated_verified_outcomes, 2);
    assert_eq!(
        candidate_monitor
            .exposures
            .iter()
            .filter(|exposure| exposure.state == ExposureState::Applied)
            .count(),
        2
    );

    let active_review = learning_review::read(repo.path()).expect("active review");
    learning_review::transition(
        repo.path(),
        &candidate_id,
        learning_review::LearningReviewState::RolledBack,
        active_review.revision,
        "live-acceptance-user",
    )
    .expect("rollback");
    let restored = RefinementAuthorityStore::open(repo.path())
        .expect("restored authority")
        .snapshot()
        .expect("restored snapshot");
    let restored_active_sha256 = digest_serialized(&restored.active);
    assert_eq!(restored_active_sha256, baseline_active_sha256);
    assert!(
        select(
            repo.path(),
            &matching_objective,
            "live-post-rollback-session"
        )
        .prompt_context
        .is_none(),
        "rolled-back behavior remained selectable"
    );

    let records_before_disabled_capture = restored.records.len();
    RefinementAuthorityStore::open(repo.path())
        .expect("privacy authority")
        .update_privacy(disabled_privacy(), 1)
        .expect("disable capture");
    let mut disabled = session(
        repo.path(),
        "disabled capture correction",
        vec![
            Message {
                role: Role::Assistant,
                content: vec![MessageBlock::Text {
                    text: "This deliberately incomplete result is only a privacy-control probe."
                        .to_owned(),
                }],
            },
            Message {
                role: Role::User,
                content: vec![MessageBlock::Text {
                    text: "You missed the privacy-control probe source.".to_owned(),
                }],
            },
        ],
    );
    disabled.completed = true;
    record_session_event(
        &mut disabled,
        Actor::Coordinator,
        EventPayload::SessionCompleted {
            report_ref: "disabled-capture-probe".to_owned(),
        },
    )
    .expect("disabled capture completion");
    let capture_disabled_blocks_new_candidate = RefinementAuthorityStore::open(repo.path())
        .expect("post-privacy authority")
        .snapshot()
        .expect("post-privacy snapshot")
        .records
        .len()
        == records_before_disabled_capture;
    assert!(capture_disabled_blocks_new_candidate);

    let credential_persisted = tree_contains(repo.path(), api_key.as_bytes());
    assert!(
        !credential_persisted,
        "live credential reached durable state"
    );
    let evidence = LiveSelfImprovementEvidence {
        schema_version: 1,
        commit: env::var("MEDUSA_LIVE_COMMIT").unwrap_or_else(|_| "local".to_owned()),
        platform: format!("{}-{}", env::consts::OS, env::consts::ARCH),
        recorded_at_unix_ms: OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000,
        provider: "minimax",
        model: model.clone(),
        live_request_count: 3,
        initial_assistant: redacted_text(&initial_assistant),
        correction: redacted_text(CORRECTION),
        follow_up_assistants: vec![redacted_text(&first.1), redacted_text(&second.1)],
        authority: AuthorityEvidence {
            proposal_id_sha256: digest_bytes(candidate_id.as_bytes()),
            proposal_version: candidate.version,
            baseline_active_sha256,
            activated_sha256,
            restored_active_sha256,
        },
        production_loop: LoopEvidence {
            correction_candidate_evaluated: true,
            approved_and_activated: true,
            matching_context_applied: true,
            repeated_verified_outcomes,
            nonmatching_task_unaffected: nonmatching_unaffected,
            rollback_restored_exact_baseline: true,
        },
        privacy: PrivacyEvidence {
            credential_persisted,
            raw_transcript_in_report: false,
            capture_disabled_blocks_new_candidate,
        },
    };
    let serialized = serde_json::to_vec_pretty(&evidence).expect("serialize evidence");
    for forbidden in [
        api_key.as_bytes(),
        initial_assistant.as_bytes(),
        first.1.as_bytes(),
        second.1.as_bytes(),
        CORRECTION.as_bytes(),
    ] {
        assert!(
            !serialized
                .windows(forbidden.len())
                .any(|window| window == forbidden),
            "sanitized report contains forbidden plaintext"
        );
    }
    if let Some(parent) = report_path.parent() {
        fs::create_dir_all(parent).expect("create report directory");
    }
    fs::write(&report_path, serialized).expect("write sanitized evidence report");
}
