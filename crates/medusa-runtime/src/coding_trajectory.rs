use std::{collections::BTreeSet, fs, path::Path, process::Command};

use medusa_agent::{AgentPlanStepStatus, AgentSession};
use medusa_protocol::EventPayload;
use medusa_provider::{MessageBlock, Role};
use medusa_session_continuity::{
    CodingTrajectoryCheckpoint, DisprovedHypothesisCheckpoint,
    ExternalEvidenceRef, FailureCheckpoint, PlanStepCheckpoint, PlanStepStatus,
    RelevantPathCheckpoint, RepositoryCheckpoint, SessionEventKind, ContinuityStore,
    VerificationOutcome, VerificationReceipt,
};
use sha2::{Digest, Sha256};

use crate::RuntimeError;

const CONTEXT_LIMIT: usize = 12_000;

pub(crate) fn sync_and_render(
    repo: &Path,
    session: &AgentSession,
    provider_native_continuation_id: Option<String>,
) -> Result<String, RuntimeError> {
    let store = store(repo, session.id.as_str());
    let mut continuity = match store.load() {
        Ok(value) => value,
        Err(medusa_session_continuity::ContinuityError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            store.create(session.id.as_str()).map_err(RuntimeError::agent)?
        }
        Err(error) => return Err(RuntimeError::agent(error)),
    };
    let previous = continuity.task.coding_trajectory.take();
    let repository = repository_checkpoint(repo);
    let mut trajectory = reduce(session, previous, repository.clone())?;
    trajectory.provider_native_continuation_id = provider_native_continuation_id;
    trajectory.validate().map_err(RuntimeError::agent)?;

    let mut task = continuity.task;
    task.plan_state = Some(format!("turn:{}", session.turn));
    task.active_step = session
        .plan
        .iter()
        .find(|step| step.status == AgentPlanStepStatus::InProgress)
        .map(|step| step.title.clone());
    task.attention_required = session.pending_question.is_some()
        || !trajectory.remaining_blockers.is_empty();
    task.verification_evidence = trajectory
        .verification_receipts
        .iter()
        .map(|receipt| format!("{}:{:?}", receipt.command, receipt.outcome))
        .collect();
    task.file_changes = trajectory.modified_files.clone();
    task.coding_trajectory = Some(trajectory.clone());

    let event_id = format!(
        "trajectory:{}:{}:{}",
        session.id,
        session.turn,
        digest_json(&trajectory)?
    );
    let outcome = store
        .project_task(
            &event_id,
            SessionEventKind::TaskStateChanged {
                state: "coding_trajectory_projected".to_owned(),
            },
            task,
        )
        .map_err(RuntimeError::agent)?;
    continuity = outcome.session().clone();
    let authoritative = continuity
        .task
        .coding_trajectory
        .as_ref()
        .ok_or_else(|| RuntimeError::agent("trajectory projection disappeared"))?;
    render(authoritative)
}

pub(crate) fn restore_for_resume(
    repo: &Path,
    session: &AgentSession,
    provider_fallback: bool,
) -> Result<Option<String>, RuntimeError> {
    let store = store(repo, session.id.as_str());
    let continuity = match store.load() {
        Ok(value) => value,
        Err(medusa_session_continuity::ContinuityError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(RuntimeError::agent(error)),
    };
    let Some(existing) = continuity.task.coding_trajectory.as_ref() else {
        return Ok(None);
    };
    let mut restored = if provider_fallback {
        existing
            .restored_for_provider_fallback()
            .map_err(RuntimeError::agent)?
    } else {
        existing.restored_for_resume().map_err(RuntimeError::agent)?
    };
    restored.invalidate_for_repository_drift(repository_checkpoint(repo));
    restored.validate().map_err(RuntimeError::agent)?;

    let mut task = continuity.task.clone();
    task.attention_required |= !restored.remaining_blockers.is_empty();
    task.verification_evidence = restored
        .verification_receipts
        .iter()
        .map(|receipt| format!("{}:{:?}", receipt.command, receipt.outcome))
        .collect();
    task.file_changes = restored.modified_files.clone();
    task.coding_trajectory = Some(restored.clone());
    let event_id = format!(
        "trajectory-resume:{}:{}:{}",
        session.id,
        restored.resume_hops,
        digest_json(&restored)?
    );
    let outcome = store
        .project_task(
            &event_id,
            SessionEventKind::TrajectoryRestored {
                resume_hops: restored.resume_hops,
            },
            task,
        )
        .map_err(RuntimeError::agent)?;
    let authoritative = outcome
        .session()
        .task
        .coding_trajectory
        .as_ref()
        .ok_or_else(|| RuntimeError::agent("restored trajectory projection disappeared"))?;
    render(authoritative).map(Some)
}

fn reduce(
    session: &AgentSession,
    previous: Option<CodingTrajectoryCheckpoint>,
    repository: RepositoryCheckpoint,
) -> Result<CodingTrajectoryCheckpoint, RuntimeError> {
    let mut trajectory = previous.unwrap_or_default();
    if trajectory.immutable_objective.is_empty() {
        trajectory.immutable_objective = session.objective.clone();
    }
    if trajectory.immutable_constraints.is_empty() {
        trajectory.immutable_constraints = initial_user_constraints(session);
    }
    if trajectory.repository.as_ref() != Some(&repository) {
        trajectory.invalidate_for_repository_drift(repository.clone());
    }
    trajectory.repository = Some(repository.clone());
    trajectory.plan_steps = session
        .plan
        .iter()
        .enumerate()
        .map(|(index, step)| PlanStepCheckpoint {
            id: format!("plan-{index}"),
            description: step.title.clone(),
            status: match step.status {
                AgentPlanStepStatus::Pending => PlanStepStatus::Pending,
                AgentPlanStepStatus::InProgress => PlanStepStatus::Active,
                AgentPlanStepStatus::Completed => PlanStepStatus::Completed,
                AgentPlanStepStatus::Failed => PlanStepStatus::Blocked,
            },
        })
        .collect();
    trajectory.unresolved_uncertainties = session
        .pending_question
        .as_ref()
        .map(|question| {
            question
                .prompts()
                .into_iter()
                .map(|item| item.question)
                .collect()
        })
        .unwrap_or_default();

    let mut modified = BTreeSet::new();
    let mut relevant = BTreeSet::new();
    let mut verification = Vec::new();
    let mut failures = Vec::new();
    let mut evidence = Vec::new();
    let mut disproved = Vec::new();
    for event in &session.events {
        match &event.payload {
            EventPayload::SessionCreated { objective } => {
                if trajectory.immutable_objective.is_empty() {
                    trajectory.immutable_objective.clone_from(objective);
                }
            }
            EventPayload::FileTransactionCommitted { paths, .. } => {
                let value = serde_json::to_value(paths).map_err(RuntimeError::agent)?;
                collect_paths(&value, &mut modified, &mut relevant);
            }
            EventPayload::ToolExecutionCompleted { .. }
            | EventPayload::WorkerEvidenceRecorded { .. }
            | EventPayload::IntegrationReceiptRecorded { .. } => {
                let serialized = serde_json::to_vec(&event.payload)
                    .map_err(RuntimeError::agent)?;
                let digest = hex_digest(&serialized);
                evidence.push(ExternalEvidenceRef {
                    id: format!("event-{}", event.sequence),
                    artifact_path: format!(
                        ".medusa/sessions/{}/journal.jsonl#{}",
                        session.id, event.sequence
                    ),
                    digest,
                });
            }
            EventPayload::VerificationCompleted { passed, evidence: lines } => {
                verification.push(VerificationReceipt {
                    command: format!("journal-verification-{}", event.sequence),
                    outcome: if *passed {
                        VerificationOutcome::Passed
                    } else {
                        VerificationOutcome::Failed
                    },
                    evidence: Some(hex_digest(
                        &serde_json::to_vec(lines).map_err(RuntimeError::agent)?,
                    )),
                });
                if !passed {
                    let summary = format!("verification failed at event {}", event.sequence);
                    let fingerprint = hex_digest(summary.as_bytes());
                    failures.push(FailureCheckpoint {
                        fingerprint: fingerprint.clone(),
                        classification: "verification".to_owned(),
                        summary,
                        repairs: Vec::new(),
                    });
                    disproved.push(DisprovedHypothesisCheckpoint {
                        signature: fingerprint,
                        repository_fingerprint: repository.workspace_fingerprint.clone(),
                    });
                }
            }
            EventPayload::RuntimeFailed { message } => {
                let fingerprint = hex_digest(message.as_bytes());
                failures.push(FailureCheckpoint {
                    fingerprint,
                    classification: "runtime".to_owned(),
                    summary: bounded(message, 1000),
                    repairs: Vec::new(),
                });
            }
            EventPayload::SessionFailed { error } => {
                let message = error.to_string();
                let fingerprint = hex_digest(message.as_bytes());
                failures.push(FailureCheckpoint {
                    fingerprint,
                    classification: "session".to_owned(),
                    summary: bounded(&message, 1000),
                    repairs: Vec::new(),
                });
            }
            _ => {}
        }
    }
    trajectory.modified_files = modified.into_iter().collect();
    trajectory.relevant_paths = relevant
        .into_iter()
        .map(|path| RelevantPathCheckpoint {
            path,
            reason: "observed in canonical mutation receipt".to_owned(),
            stale: false,
        })
        .collect();
    trajectory.verification_receipts = verification;
    trajectory.failure_history = dedupe_failures(failures);
    trajectory.external_evidence_refs = evidence.into_iter().rev().take(128).collect();
    trajectory.disproved_hypotheses = disproved;
    trajectory.verification_requirements = trajectory
        .verification_receipts
        .iter()
        .map(|receipt| receipt.command.clone())
        .collect();
    trajectory.remaining_blockers.retain(|item| {
        item != "repository drift requires trajectory revalidation"
    });
    if trajectory
        .verification_receipts
        .last()
        .is_some_and(|receipt| receipt.outcome == VerificationOutcome::Failed)
    {
        trajectory
            .remaining_blockers
            .push("latest authoritative verification failed".to_owned());
    }
    trajectory.continuation_intent = session
        .plan
        .iter()
        .find(|step| step.status != AgentPlanStepStatus::Completed)
        .map(|step| format!("continue plan step: {}", step.title));
    Ok(trajectory)
}

fn initial_user_constraints(session: &AgentSession) -> Vec<String> {
    session
        .messages
        .iter()
        .filter(|message| message.role == Role::User)
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            MessageBlock::Text { text } if !text.trim().is_empty() => {
                Some(bounded(text, 1500))
            }
            _ => None,
        })
        .take(32)
        .collect()
}

fn collect_paths(
    value: &serde_json::Value,
    modified: &mut BTreeSet<String>,
    relevant: &mut BTreeSet<String>,
) {
    match value {
        serde_json::Value::String(value)
            if value.contains('/') || value.contains('\\') =>
        {
            let normalized = value.replace('\\', "/");
            modified.insert(normalized.clone());
            relevant.insert(normalized);
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_paths(value, modified, relevant);
            }
        }
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                if matches!(key.as_str(), "path" | "file" | "files" | "changed_paths" | "modified_files") {
                    collect_paths(value, modified, relevant);
                }
            }
        }
        _ => {}
    }
}

fn dedupe_failures(values: Vec<FailureCheckpoint>) -> Vec<FailureCheckpoint> {
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .filter(|failure| seen.insert(failure.fingerprint.clone()))
        .take(128)
        .collect()
}

fn repository_checkpoint(repo: &Path) -> RepositoryCheckpoint {
    let head = git(repo, &["rev-parse", "HEAD"]);
    let workspace = git(
        repo,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )
    .unwrap_or_else(|| {
        fs::metadata(repo)
            .map(|meta| format!("{}:{}", repo.display(), meta.len()))
            .unwrap_or_else(|_| repo.display().to_string())
    });
    RepositoryCheckpoint {
        head,
        workspace_fingerprint: hex_digest(workspace.as_bytes()),
    }
}

fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn render(trajectory: &CodingTrajectoryCheckpoint) -> Result<String, RuntimeError> {
    let json = serde_json::to_string(trajectory).map_err(RuntimeError::agent)?;
    Ok(format!(
        "[medusa-coding-trajectory-v1]\nAuthoritative compact trajectory derived from the canonical journal. Preserve immutable objective/constraints, continue from retained plan/failure/verification state, do not retry disproved hypotheses on the same repository fingerprint without new evidence, and revalidate stale paths after repository drift.\n{}",
        bounded(&json, CONTEXT_LIMIT)
    ))
}

fn digest_json(value: &CodingTrajectoryCheckpoint) -> Result<String, RuntimeError> {
    serde_json::to_vec(value)
        .map(|bytes| hex_digest(&bytes))
        .map_err(RuntimeError::agent)
}

fn hex_digest(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn bounded(value: &str, max: usize) -> String {
    let mut output = value.chars().take(max).collect::<String>();
    if value.chars().count() > max {
        output.push('…');
    }
    output
}

fn store(repo: &Path, session_id: &str) -> ContinuityStore {
    ContinuityStore::new(
        repo.join(".medusa/continuity")
            .join(format!("{session_id}.json")),
    )
}

#[cfg(test)]
mod tests {
    use medusa_agent::AgentEngine;
    use medusa_config::Config;
    use medusa_core::MedusaResult;
    use medusa_protocol::{Actor, EventPayload};
    use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};

    use super::*;

    struct UnusedProvider;
    impl ModelProvider for UnusedProvider {
        fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {
            unreachable!("not used")
        }
    }

    #[test]
    fn journal_projection_survives_restart_and_repository_drift() {
        let repo = tempfile::tempdir().expect("repo");
        Command::new("git").arg("init").arg(repo.path()).status().expect("git init");
        Command::new("git").arg("-C").arg(repo.path()).args(["config", "user.email", "test@example.invalid"]).status().expect("email");
        Command::new("git").arg("-C").arg(repo.path()).args(["config", "user.name", "Medusa Test"]).status().expect("name");
        fs::write(repo.path().join("tracked.txt"), "one").expect("write");
        Command::new("git").arg("-C").arg(repo.path()).args(["add", "."]).status().expect("add");
        Command::new("git").arg("-C").arg(repo.path()).args(["commit", "-m", "initial"]).status().expect("commit");
        let engine = AgentEngine::new(UnusedProvider, Config::default());
        let mut session = engine.create_session(repo.path(), "repair regression".to_owned()).expect("session");
        engine.append_user_message(&mut session, vec![MessageBlock::Text { text: "keep public API stable".to_owned() }]).expect("constraint");
        medusa_agent::record_session_event(&mut session, Actor::System("verifier".to_owned()), EventPayload::VerificationCompleted { passed: false, evidence: vec!["regression failed".to_owned()] }).expect("verification");
        let first = sync_and_render(repo.path(), &session, Some("provider-native-1".to_owned())).expect("sync");
        assert!(first.contains("keep public API stable"));
        assert!(first.contains("verification"));
        medusa_agent::compact_session(&mut session, Some("repair regression"))
            .expect("forced compaction");
        let restored = restore_for_resume(repo.path(), &session, true).expect("restore").expect("context");
        assert!(restored.contains("repair regression"));
        assert!(restored.contains("keep public API stable"));
        assert!(!restored.contains("provider-native-1"));
        let stored = store(repo.path(), session.id.as_str()).load().expect("stored resume");
        assert_eq!(stored.task.coding_trajectory.as_ref().expect("trajectory").resume_hops, 1);
        fs::write(repo.path().join("tracked.txt"), "drift").expect("drift");
        let drifted = restore_for_resume(repo.path(), &session, false).expect("drift restore").expect("context");
        assert!(drifted.contains("repository drift requires trajectory revalidation"));
        let drifted_stored = store(repo.path(), session.id.as_str()).load().expect("stored drift");
        let drifted_trajectory = drifted_stored.task.coding_trajectory.as_ref().expect("trajectory");
        assert_eq!(drifted_trajectory.resume_hops, 2);
        assert!(drifted_trajectory.relevant_paths.iter().all(|path| path.stale));
    }
}
