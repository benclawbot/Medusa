use std::{
    fs,
    path::{Path, PathBuf},
};

use medusa_context::refinement::{
    EvidenceKind, EvidenceRef, ProposerMetadata, RefinementArtifactKind, RefinementContent,
    RefinementProposal, RefinementRisk, RefinementScope,
};
use medusa_core::{MedusaResult, learning_policy::LearningAdmissionPolicy};
use medusa_improvement::{
    correction_loop::{
        CorrectionLoopEngine, CorrectionLoopRequest, DeterministicProductionReplayRunner,
    },
    correction_signals::{ConversationRole, ConversationTurn},
    provenance::{
        ProvenanceGraph, ProvenanceGraphStore, ProvenanceOutcome, ProvenanceSource,
        repository_identity, repository_revision,
    },
    refinement_authority::RefinementAuthorityStore,
};
use medusa_protocol::{Actor, EventPayload};
use medusa_provider::{MessageBlock, Role};
use serde_json::{Value, json};

use super::{AgentSession, lessons, skill_drafts, skill_outcomes, skill_probation};

const MIN_PROBATION_CONFIDENCE_MILLI: u64 = 750;

pub(super) fn process(session: &AgentSession) -> MedusaResult<()> {
    let policy = policy_for(&session.repo)?;
    if !policy.capture_enabled() {
        return Ok(());
    }

    let provenance = persist_provenance(session, &policy)?;
    let correction_report = if session.completed {
        process_correction_loop(session, &policy, provenance.clone())?
    } else {
        medusa_improvement::correction_loop::CorrectionLoopReport::default()
    };
    if !session.completed {
        return Ok(());
    }

    if !authoritative_success(session) {
        return Ok(());
    }

    let marker = processed_marker(session);
    if marker.is_file() {
        return Ok(());
    }

    if correction_report.episodes.is_empty()
        && policy.automatic_proposals_enabled()
        && let Some(proposal_path) = lessons::extract_completed_session(session)?
    {
        let canonical_path = admit_to_canonical_memory(session, &proposal_path)?;
        let value: Value = serde_json::from_slice(&fs::read(&canonical_path)?)?;
        if value["lifecycle"]["status"] == "probation" {
            skill_drafts::create_from_lesson(&canonical_path)?;
        }
        skill_probation::refresh(&session.repo)?;
    }

    if policy.telemetry_enabled() {
        skill_outcomes::record_completed_session(session)?;
    }

    write_json_atomic(
        &marker,
        &json!({
            "schema_version": 2,
            "session_id": session.id.to_string(),
            "completed": true,
            "authoritative_success": true,
            "automatic_proposals_enabled": policy.automatic_proposals_enabled(),
            "telemetry_enabled": policy.telemetry_enabled(),
            "provenance_head_digest": provenance.head_digest,
            "provenance_observation_count": provenance.observations.len(),
            "authority_receipts": authority_receipts(session),
        }),
    )
}

fn process_correction_loop(
    session: &AgentSession,
    policy: &LearningAdmissionPolicy,
    provenance: ProvenanceGraph,
) -> MedusaResult<medusa_improvement::correction_loop::CorrectionLoopReport> {
    let turns = correction_turns(session);
    if turns.is_empty() {
        return Ok(medusa_improvement::correction_loop::CorrectionLoopReport::default());
    }
    let tool_capabilities = provenance
        .tool_observations()
        .filter_map(|observation| observation.tool_name.clone())
        .collect::<Vec<_>>();
    let request = CorrectionLoopRequest {
        session_id: session.id.to_string(),
        objective: session.objective.clone(),
        turns,
        provenance,
        policy: policy.clone(),
        repository_fixture: format!(
            "repository revision {}",
            repository_revision(&session.repo).unwrap_or_else(|| "unknown".to_owned())
        ),
        tool_capabilities,
        now_unix_ms: session.updated_at.unix_timestamp_nanos() as i64 / 1_000_000,
    };
    let report = CorrectionLoopEngine::default()
        .run(&session.repo, request, &DeterministicProductionReplayRunner)
        .map_err(|error| {
            medusa_core::MedusaError::new(
                medusa_core::ErrorCode::PersistenceFailed,
                medusa_core::ErrorCategory::Persistence,
                format!("correction-to-improvement loop failed: {error}"),
            )
        })?;
    Ok(report)
}

fn correction_turns(session: &AgentSession) -> Vec<ConversationTurn> {
    session
        .messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| {
            let mut text = String::new();
            for block in &message.content {
                let MessageBlock::Text { text: value } = block else {
                    return None;
                };
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(value);
            }
            if text.trim().is_empty() {
                return None;
            }
            let role = match message.role {
                Role::User => ConversationRole::User,
                Role::Assistant => ConversationRole::Assistant,
            };
            Some(ConversationTurn {
                id: format!("message-{index}"),
                role,
                content: text,
            })
        })
        .collect()
}

pub(super) fn provenance_graph(session: &AgentSession) -> MedusaResult<ProvenanceGraph> {
    let policy = policy_for(&session.repo)?;
    if !policy.capture_enabled() {
        return Ok(ProvenanceGraph::empty());
    }
    let mut graph = ProvenanceGraph::empty();
    let repository = repository_identity(&session.repo);
    let revision = repository_revision(&session.repo);
    for event in &session.events {
        graph
            .ingest_event(
                event,
                &session.id.to_string(),
                repository.clone(),
                revision.clone(),
                &policy,
                session.updated_at,
            )
            .map_err(|error| {
                medusa_core::MedusaError::new(
                    medusa_core::ErrorCode::PersistenceFailed,
                    medusa_core::ErrorCategory::Persistence,
                    format!("typed provenance rejected event: {error}"),
                )
            })?;
    }
    Ok(graph)
}

fn persist_provenance(
    session: &AgentSession,
    policy: &LearningAdmissionPolicy,
) -> MedusaResult<ProvenanceGraph> {
    let mut store = ProvenanceGraphStore::open(&session.repo).map_err(|error| {
        medusa_core::MedusaError::new(
            medusa_core::ErrorCode::PersistenceFailed,
            medusa_core::ErrorCategory::Persistence,
            format!("typed provenance store unavailable: {error}"),
        )
    })?;
    store
        .ingest_events(
            &session.events,
            &session.id.to_string(),
            repository_identity(&session.repo),
            repository_revision(&session.repo),
            policy,
            session.updated_at,
        )
        .map_err(|error| {
            medusa_core::MedusaError::new(
                medusa_core::ErrorCode::PersistenceFailed,
                medusa_core::ErrorCategory::Persistence,
                format!("typed provenance ingestion failed: {error}"),
            )
        })?;
    Ok(store.graph().clone())
}

pub(super) fn policy_for(repo: &Path) -> MedusaResult<LearningAdmissionPolicy> {
    LearningAdmissionPolicy::for_repository(repo)
}

pub(super) fn telemetry_allowed(repo: &Path) -> MedusaResult<bool> {
    Ok(policy_for(repo).is_ok_and(|policy| policy.telemetry_enabled()))
}

/// Positive learning is admitted only from a root task with authoritative verification that
/// remains valid through terminal completion. Direct sessions carry a `VerificationCompleted`
/// receipt. The coordinated mutation path currently records its successful independent
/// verification, authorization, integration, and reconciliation as `IntegrationReceiptRecorded`;
/// that canonical receipt is accepted until #820 gives every root trajectory a single typed
/// verification identity. Production-created teammate objectives remain non-root.
pub(super) fn authoritative_success(session: &AgentSession) -> bool {
    if !session.completed || delegated_worker_session(session) {
        return false;
    }

    let Some(completion_sequence) = session.events.iter().rev().find_map(|event| {
        matches!(&event.payload, EventPayload::SessionCompleted { .. }).then_some(event.sequence)
    }) else {
        return false;
    };

    let explicit_verification = session.events.iter().rev().find_map(|event| {
        if event.sequence > completion_sequence {
            return None;
        }
        match &event.payload {
            EventPayload::VerificationCompleted { passed, .. } => Some((event.sequence, *passed)),
            _ => None,
        }
    });

    let verification_sequence = match explicit_verification {
        Some((sequence, true)) => sequence,
        Some((_, false)) => return false,
        None => {
            let Some(sequence) = session.events.iter().rev().find_map(|event| {
                (event.sequence <= completion_sequence
                    && matches!(
                        &event.payload,
                        EventPayload::IntegrationReceiptRecorded { .. }
                    ))
                .then_some(event.sequence)
            }) else {
                return false;
            };
            sequence
        }
    };

    !session.events.iter().any(|event| {
        event.sequence >= verification_sequence
            && matches!(
                &event.payload,
                EventPayload::SessionFailed { .. }
                    | EventPayload::RuntimeFailed { .. }
                    | EventPayload::CancellationCompleted
            )
    })
}

fn delegated_worker_session(session: &AgentSession) -> bool {
    if session
        .events
        .iter()
        .any(|event| matches!(&event.actor, Actor::Worker(_)))
    {
        return true;
    }
    let objective = session.objective.trim_start();
    objective.starts_with("Implement delegated task `")
        || objective.starts_with("Collect read-only repository evidence for the parent goal.")
        || objective
            .starts_with("Perform a read-only risk and failure-mode review for the parent goal.")
}

fn admit_to_canonical_memory(
    session: &AgentSession,
    proposal_path: &Path,
) -> MedusaResult<PathBuf> {
    let mut proposal: Value = serde_json::from_slice(&fs::read(proposal_path)?)?;
    let confidence = proposal["confidence_milli"].as_u64().unwrap_or_default();
    let all_evidence = authoritative_evidence(session);
    let safe_evidence = all_evidence
        .iter()
        .filter(|item| !secret_like(item))
        .cloned()
        .collect::<Vec<_>>();
    let safe = !safe_evidence.is_empty() && safe_evidence.len() == all_evidence.len();
    let status = if safe && confidence >= MIN_PROBATION_CONFIDENCE_MILLI {
        "probation"
    } else {
        "rejected"
    };

    proposal["lifecycle"] = json!({
        "status": status,
        "auto_promotion": "disabled",
        "promotion": {
            "mode": "explicit_graduation",
            "command": "medusa skills graduate NAME --confirm",
            "requires_probation_state": "passed"
        },
        "rollback": {
            "mode": "graduation_receipt_transaction",
            "on_receipt_failure": "restore_previous_lifecycle_state"
        },
        "minimum_confidence_milli": MIN_PROBATION_CONFIDENCE_MILLI,
        "rejection_reason": if status == "rejected" {
            "insufficient confidence, evidence, or safety"
        } else {
            ""
        },
    });
    proposal["provenance"] = json!({
        "session_id": session.id.to_string(),
        "evidence": safe_evidence,
        "evidence_count": all_evidence.len(),
        "verification_result": "verified",
        "authority_receipts": authority_receipts(session),
        "completed_at": session.updated_at,
    });

    admit_to_refinement_authority(session, &proposal, &safe_evidence)?;

    let path = session
        .repo
        .join(".medusa/memory/lessons")
        .join(format!("{}.json", session.id));
    write_json_atomic(&path, &proposal)?;
    Ok(path)
}

fn admit_to_refinement_authority(
    session: &AgentSession,
    lesson: &Value,
    evidence: &[String],
) -> MedusaResult<()> {
    let summary = lesson
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("verified completed-session learning");
    let procedure = lesson
        .get("procedure")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter(|value| !secret_like(value))
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_default();
    let body = if procedure.trim().is_empty() {
        summary.to_owned()
    } else {
        format!("{summary} Procedure: {procedure}")
    };
    if body.trim().is_empty() || evidence.is_empty() {
        return Ok(());
    }
    let mut authority = RefinementAuthorityStore::open(&session.repo).map_err(|error| {
        medusa_core::MedusaError::new(
            medusa_core::ErrorCode::PersistenceFailed,
            medusa_core::ErrorCategory::Persistence,
            format!("canonical refinement authority unavailable: {error}"),
        )
    })?;
    let sequence = session
        .events
        .last()
        .map_or(1, |event| event.sequence.max(1));
    let session_id = session.id.to_string();
    let proposal_id = lesson
        .get("id")
        .and_then(Value::as_str)
        .map_or_else(|| session_id.clone(), str::to_owned);
    let proposal = RefinementProposal {
        id: proposal_id.clone(),
        version: 1,
        artifact_kind: RefinementArtifactKind::RepositoryConvention,
        scope: RefinementScope::Repository,
        evidence: provenance_evidence_refs(session, sequence),
        before: None,
        after: RefinementContent::RepositoryConvention {
            key: format!("lesson.{proposal_id}"),
            value: body,
        },
        rationale: "admitted from an authoritatively verified completed session".to_owned(),
        expected_outcome: "matching future work can review this verified workflow candidate"
            .to_owned(),
        proposer: ProposerMetadata {
            model: "medusa-agent".to_owned(),
            route: "completed-session".to_owned(),
            version: "1".to_owned(),
        },
        risk: RefinementRisk::Low,
    };
    let revision = authority
        .snapshot()
        .map_err(|error| {
            medusa_core::MedusaError::new(
                medusa_core::ErrorCode::PersistenceFailed,
                medusa_core::ErrorCategory::Persistence,
                format!("canonical refinement snapshot unavailable: {error}"),
            )
        })?
        .revision;
    match authority.propose(proposal, revision) {
        Ok(_) => Ok(()),
        Err(medusa_improvement::refinement_authority::RefinementAuthorityError::Conflict {
            ..
        }) => Ok(()),
        Err(error) => Err(medusa_core::MedusaError::new(
            medusa_core::ErrorCode::PersistenceFailed,
            medusa_core::ErrorCategory::Persistence,
            format!("canonical refinement proposal rejected: {error}"),
        )),
    }
}

fn provenance_evidence_refs(session: &AgentSession, sequence: u64) -> Vec<EvidenceRef> {
    let references = provenance_graph(session)
        .ok()
        .map(|graph| {
            graph
                .observations
                .iter()
                .take(12)
                .map(|observation| EvidenceRef {
                    id: observation.id.clone(),
                    kind: match observation.source {
                        ProvenanceSource::UserCorrection => EvidenceKind::UserCorrection,
                        ProvenanceSource::Verification
                        | ProvenanceSource::Integration
                        | ProvenanceSource::Recovery
                        | ProvenanceSource::TerminalOutcome => EvidenceKind::ExplicitOutcome,
                        _ => EvidenceKind::ToolEvent,
                    },
                    trajectory_id: observation.trajectory_id.clone(),
                    start_sequence: observation.source_range.start_sequence,
                    end_sequence: observation.source_range.end_sequence,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if references.is_empty() {
        return vec![EvidenceRef {
            id: format!("completed-session-evidence-{}", session.id),
            kind: EvidenceKind::ToolEvent,
            trajectory_id: session.id.to_string(),
            start_sequence: 1,
            end_sequence: sequence,
        }];
    }
    references
}

pub(super) fn authoritative_evidence(session: &AgentSession) -> Vec<String> {
    let mut evidence = session.evidence.clone();
    for event in &session.events {
        match &event.payload {
            EventPayload::VerificationCompleted {
                passed: true,
                evidence: verification_evidence,
            } => evidence.extend(verification_evidence.iter().cloned()),
            EventPayload::IntegrationReceiptRecorded { receipt } => {
                let summary = receipt.get("commit").and_then(Value::as_str).map_or_else(
                    || "independent verification, integration, and reconciliation completed".to_owned(),
                    |commit| {
                        format!(
                            "independent verification, integration, and reconciliation completed for commit {commit}"
                        )
                    },
                );
                evidence.push(summary);
            }
            _ => {}
        }
    }
    if let Ok(graph) = provenance_graph(session) {
        for observation in graph.observations.iter().filter(|observation| {
            matches!(
                observation.source,
                ProvenanceSource::Verification
                    | ProvenanceSource::Integration
                    | ProvenanceSource::TerminalOutcome
            ) && observation.outcome == ProvenanceOutcome::Positive
        }) {
            evidence.push(format!(
                "{} [provenance:{}]",
                observation.summary, observation.id
            ));
        }
    }
    evidence.sort();
    evidence.dedup();
    evidence
}

fn authority_receipts(session: &AgentSession) -> Vec<Value> {
    session
        .events
        .iter()
        .filter_map(|event| {
            let kind = match &event.payload {
                EventPayload::WorkerEvidenceRecorded { .. } => "worker_evidence",
                EventPayload::IntegrationReceiptRecorded { .. } => "integration",
                EventPayload::VerificationCompleted { passed: true, .. } => "verification",
                EventPayload::SessionCompleted { .. } => "completion",
                _ => return None,
            };
            Some(json!({
                "kind": kind,
                "event_id": event.event_id.to_string(),
                "sequence": event.sequence,
            }))
        })
        .collect()
}

fn processed_marker(session: &AgentSession) -> PathBuf {
    session
        .repo
        .join(".medusa/learning/processed-sessions")
        .join(format!("{}.json", session.id))
}

fn secret_like(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "api_key",
        "apikey",
        "authorization:",
        "bearer ",
        "secret=",
        "token=",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn write_json_atomic(path: &Path, value: &Value) -> MedusaResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use medusa_core::SessionId;
    use medusa_protocol::{Actor, EventPayload};
    use time::OffsetDateTime;

    use crate::evidence::append_event;

    use super::*;

    fn session(repo: &Path) -> AgentSession {
        let mut session = AgentSession {
            id: SessionId::new(),
            objective: "Fix and verify the repository".to_owned(),
            repo: repo.to_path_buf(),
            created_at: OffsetDateTime::now_utc(),
            updated_at: OffsetDateTime::now_utc(),
            completed: true,
            turn: 3,
            plan: Vec::new(),
            pending_question: None,
            messages: Vec::new(),
            events: Vec::new(),
            evidence: vec![
                "cargo test --workspace passed".to_owned(),
                "release smoke passed".to_owned(),
            ],
            tool_artifacts: Vec::new(),
            world_model: None,
            approval_grants: Vec::new(),
            approval_receipts: Vec::new(),
            rollback_receipts: Vec::new(),
        };
        append_event(
            &mut session,
            Actor::System("test".to_owned()),
            EventPayload::VerificationCompleted {
                passed: true,
                evidence: vec!["cargo test --workspace passed".to_owned()],
            },
        )
        .expect("verification event");
        append_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::SessionCompleted {
                report_ref: "test-report".to_owned(),
            },
        )
        .expect("completion event");
        session
    }

    fn update_privacy(repo: &Path, privacy: medusa_core::learning_policy::LearningPrivacyPolicy) {
        let root = repo.join(".medusa/learning-review");
        fs::create_dir_all(&root).expect("privacy root");
        fs::write(
            root.join("state.json"),
            serde_json::to_vec_pretty(&json!({
                "schema_version": 1,
                "revision": 1,
                "privacy": privacy,
                "items": [],
                "audit_head": "0000000000000000000000000000000000000000000000000000000000000000"
            }))
            .expect("privacy json"),
        )
        .expect("privacy state");
    }

    fn make_coordinated_reconciled(session: &mut AgentSession) {
        session.events.clear();
        append_event(
            session,
            Actor::Coordinator,
            EventPayload::WorkerEvidenceRecorded {
                evidence: json!({"commit": "abc123", "reviewed": true}),
            },
        )
        .expect("worker evidence");
        append_event(
            session,
            Actor::Coordinator,
            EventPayload::IntegrationReceiptRecorded {
                receipt: json!({
                    "commit": "abc123",
                    "independent_verification": "passed",
                    "reconciled": true
                }),
            },
        )
        .expect("integration receipt");
        append_event(
            session,
            Actor::Coordinator,
            EventPayload::SessionCompleted {
                report_ref: "commit:abc123".to_owned(),
            },
        )
        .expect("completion");
    }

    #[test]
    fn processing_is_idempotent_and_writes_authority_receipts() {
        let repo = tempfile::tempdir().expect("repo");
        let session = session(repo.path());
        process(&session).expect("first processing");
        process(&session).expect("retry processing");

        let memory = repo
            .path()
            .join(".medusa/memory/lessons")
            .join(format!("{}.json", session.id));
        let value: Value =
            serde_json::from_slice(&fs::read(memory).expect("memory")).expect("memory json");
        assert_eq!(value["provenance"]["session_id"], session.id.to_string());
        assert_eq!(value["provenance"]["verification_result"], "verified");
        assert_eq!(value["lifecycle"]["status"], "probation");
        assert!(
            value["provenance"]["authority_receipts"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["kind"] == "completion"))
        );
        let authority =
            medusa_improvement::refinement_authority::RefinementAuthorityStore::open(repo.path())
                .expect("canonical authority");
        let canonical = authority.snapshot().expect("canonical snapshot");
        assert_eq!(canonical.records.len(), 1);
        assert!(canonical.active.is_empty());
        assert!(processed_marker(&session).is_file());
    }

    #[test]
    fn completed_session_without_authoritative_verification_is_ineligible() {
        let repo = tempfile::tempdir().expect("repo");
        let mut session = session(repo.path());
        session
            .events
            .retain(|event| !matches!(&event.payload, EventPayload::VerificationCompleted { .. }));
        assert!(!authoritative_success(&session));
        process(&session).expect("blocked process");
        assert!(!processed_marker(&session).exists());
        assert!(!repo.path().join(".medusa/memory/lessons").exists());
        assert!(
            !repo
                .path()
                .join(".medusa/refinement-authority/journal.json")
                .exists()
        );
    }

    #[test]
    fn coordinated_reconciled_parent_is_authoritative_and_can_propose() {
        let repo = tempfile::tempdir().expect("repo");
        let mut session = session(repo.path());
        session.evidence.clear();
        make_coordinated_reconciled(&mut session);
        assert!(authoritative_success(&session));
        let evidence = authoritative_evidence(&session);
        assert!(evidence.iter().any(|item| item.contains("commit abc123")));
        assert!(
            lessons::extract_completed_session(&session)
                .expect("extract")
                .is_some()
        );
    }

    #[test]
    fn failed_verification_cannot_be_overridden_by_completion_or_integration() {
        let repo = tempfile::tempdir().expect("repo");
        let mut session = session(repo.path());
        session.events.clear();
        append_event(
            &mut session,
            Actor::System("test".to_owned()),
            EventPayload::VerificationCompleted {
                passed: false,
                evidence: vec!["tests failed".to_owned()],
            },
        )
        .expect("failed verification");
        append_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::IntegrationReceiptRecorded {
                receipt: json!({"commit": "fabricated"}),
            },
        )
        .expect("integration");
        append_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::SessionCompleted {
                report_ref: "fabricated-completion".to_owned(),
            },
        )
        .expect("completion");
        assert!(!authoritative_success(&session));
    }

    #[test]
    fn corrupt_privacy_disables_optional_telemetry_without_error() {
        let repo = tempfile::tempdir().expect("repo");
        let root = repo.path().join(".medusa/learning-review");
        fs::create_dir_all(&root).expect("privacy root");
        fs::write(root.join("state.json"), b"not-json").expect("privacy state");
        assert!(!telemetry_allowed(repo.path()).expect("optional telemetry decision"));
    }

    #[test]
    fn production_worker_objective_cannot_become_positive_learning() {
        let repo = tempfile::tempdir().expect("repo");
        let mut session = session(repo.path());
        session.objective = "Implement delegated task `implementation` inside this isolated Git worktree. Objective: fix it.".to_owned();
        assert!(!authoritative_success(&session));
        process(&session).expect("blocked worker learning");
        assert!(!processed_marker(&session).exists());
    }

    #[test]
    fn capture_disabled_persists_no_learning_artifact() {
        let repo = tempfile::tempdir().expect("repo");
        update_privacy(
            repo.path(),
            medusa_core::learning_policy::LearningPrivacyPolicy {
                capture_enabled: false,
                user_persistence_enabled: true,
                cross_repository_reuse_enabled: true,
                telemetry_enabled: true,
                automatic_proposals_enabled: true,
            },
        );
        let mut session = session(repo.path());
        session.objective = "SEEDED_PRIVATE_CONTENT".to_owned();
        process(&session).expect("privacy block");
        assert!(!repo.path().join(".medusa/learning/proposals").exists());
        assert!(!repo.path().join(".medusa/memory/lessons").exists());
        assert!(!processed_marker(&session).exists());
    }

    #[test]
    fn automatic_proposals_disabled_creates_no_candidate() {
        let repo = tempfile::tempdir().expect("repo");
        update_privacy(
            repo.path(),
            medusa_core::learning_policy::LearningPrivacyPolicy {
                capture_enabled: true,
                user_persistence_enabled: false,
                cross_repository_reuse_enabled: false,
                telemetry_enabled: false,
                automatic_proposals_enabled: false,
            },
        );
        let session = session(repo.path());
        process(&session).expect("process");
        assert!(!repo.path().join(".medusa/learning/proposals").exists());
        assert!(!repo.path().join(".medusa/memory/lessons").exists());
        assert!(processed_marker(&session).is_file());
    }

    #[test]
    fn secret_like_evidence_is_rejected_and_not_persisted() {
        let repo = tempfile::tempdir().expect("repo");
        let mut session = session(repo.path());
        session.evidence.push("token=do-not-store".to_owned());
        let proposal = lessons::extract_completed_session(&session)
            .expect("extract")
            .expect("proposal");
        let memory = admit_to_canonical_memory(&session, &proposal).expect("memory");
        let content = fs::read_to_string(&memory).expect("memory file");
        let value: Value = serde_json::from_str(&content).expect("memory json");
        assert_eq!(value["lifecycle"]["status"], "rejected");
        assert!(!content.contains("do-not-store"));
    }

    #[test]
    fn user_correction_creates_reviewable_candidate_without_activation() {
        let repo = tempfile::tempdir().expect("repo");
        let mut session = session(repo.path());
        session.messages = vec![
            medusa_provider::Message {
                role: medusa_provider::Role::Assistant,
                content: vec![medusa_provider::MessageBlock::Text {
                    text: "I claimed the source inventory was complete.".into(),
                }],
            },
            medusa_provider::Message {
                role: medusa_provider::Role::User,
                content: vec![medusa_provider::MessageBlock::Text {
                    text: "You missed coverage of the authoritative sources.".into(),
                }],
            },
        ];
        append_event(
            &mut session,
            Actor::User,
            EventPayload::UserPromptReceived {
                text: "You missed coverage of the authoritative sources.".into(),
            },
        )
        .expect("correction event");
        process(&session).expect("correction loop");
        let state: serde_json::Value = serde_json::from_slice(
            &fs::read(repo.path().join(".medusa/correction-loop/state.json"))
                .expect("correction-loop state"),
        )
        .expect("state json");
        assert_eq!(state["episodes"][0]["state"], "awaiting_review");
        let authority = RefinementAuthorityStore::open(repo.path()).expect("authority");
        let snapshot = authority.snapshot().expect("authority snapshot");
        assert_eq!(snapshot.records.len(), 1);
        assert!(snapshot.active.is_empty());
    }
}
