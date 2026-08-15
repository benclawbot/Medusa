use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_provider::{MessageBlock, Role};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::session::{AgentPlanStepStatus, AgentSession};

const SCHEMA_VERSION: u16 = 1;
const MAX_CONSTRAINTS: usize = 64;
const MAX_FAILURES: usize = 128;
const MAX_EVIDENCE_REFS: usize = 256;
const MAX_RENDER_CHARS: usize = 24_000;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TrajectoryPlanStep {
    pub title: String,
    pub status: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TrajectoryFailure {
    pub fingerprint: String,
    pub summary: String,
    pub occurrences: u32,
    pub resolved: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TrajectoryRepositoryIdentity {
    pub head: Option<String>,
    pub workspace_fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CodingTrajectoryState {
    pub schema_version: u16,
    pub session_id: String,
    pub objective: String,
    pub user_constraints: Vec<String>,
    pub plan: Vec<TrajectoryPlanStep>,
    pub relevant_paths: BTreeMap<String, String>,
    pub changed_paths: BTreeSet<String>,
    pub mutation_generation: u64,
    pub verification_receipts: Vec<String>,
    pub failures: Vec<TrajectoryFailure>,
    pub disproved_hypotheses: BTreeSet<String>,
    pub unresolved_questions: Vec<String>,
    pub remaining_blockers: Vec<String>,
    pub evidence_refs: BTreeSet<String>,
    pub repository: TrajectoryRepositoryIdentity,
    pub repository_drift_detected: bool,
}

impl CodingTrajectoryState {
    fn new(session: &AgentSession) -> MedusaResult<Self> {
        Ok(Self {
            schema_version: SCHEMA_VERSION,
            session_id: session.id.as_str().to_owned(),
            objective: session.objective.clone(),
            user_constraints: Vec::new(),
            plan: Vec::new(),
            relevant_paths: BTreeMap::new(),
            changed_paths: BTreeSet::new(),
            mutation_generation: 0,
            verification_receipts: Vec::new(),
            failures: Vec::new(),
            disproved_hypotheses: BTreeSet::new(),
            unresolved_questions: Vec::new(),
            remaining_blockers: Vec::new(),
            evidence_refs: BTreeSet::new(),
            repository: repository_identity(&session.repo)?,
            repository_drift_detected: false,
        })
    }

    fn synchronize(&mut self, session: &AgentSession) -> MedusaResult<()> {
        if self.schema_version != SCHEMA_VERSION || self.session_id != session.id.as_str() {
            return Err(invalid("coding trajectory state is stale or corrupted"));
        }
        let current = repository_identity(&session.repo)?;
        if self.repository != current {
            self.repository_drift_detected = true;
            self.verification_receipts.clear();
            push_unique(
                &mut self.remaining_blockers,
                "repository drift invalidated prior verification and affected assumptions"
                    .to_owned(),
                64,
            );
        }
        self.repository = current;
        self.objective.clone_from(&session.objective);
        self.plan = session
            .plan
            .iter()
            .map(|step| TrajectoryPlanStep {
                title: step.title.clone(),
                status: match step.status {
                    AgentPlanStepStatus::Pending => "pending",
                    AgentPlanStepStatus::InProgress => "in_progress",
                    AgentPlanStepStatus::Completed => "completed",
                    AgentPlanStepStatus::Failed => "failed",
                }
                .to_owned(),
            })
            .collect();
        self.unresolved_questions = session
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
        collect_user_constraints(session, &mut self.user_constraints);
        collect_evidence(session, self);
        Ok(())
    }

    fn observe_mutation(&mut self, session: &AgentSession, paths: &[String]) -> MedusaResult<()> {
        self.objective.clone_from(&session.objective);
        self.plan = session
            .plan
            .iter()
            .map(|step| TrajectoryPlanStep {
                title: step.title.clone(),
                status: format!("{:?}", step.status).to_ascii_lowercase(),
            })
            .collect();
        for path in paths.iter().filter(|path| !path.trim().is_empty()) {
            self.changed_paths.insert(path.clone());
            self.relevant_paths
                .entry(path.clone())
                .or_insert_with(|| "authoritative mutation receipt".to_owned());
        }
        self.mutation_generation = self.mutation_generation.saturating_add(1);
        self.verification_receipts.clear();
        self.repository = repository_identity(&session.repo)?;
        self.repository_drift_detected = false;
        self.remaining_blockers
            .retain(|item| !item.starts_with("repository drift invalidated"));
        collect_user_constraints(session, &mut self.user_constraints);
        collect_evidence(session, self);
        Ok(())
    }

    fn observe_verification(
        &mut self,
        session: &AgentSession,
        summary: &[String],
        passed: bool,
    ) -> MedusaResult<()> {
        self.repository = repository_identity(&session.repo)?;
        self.repository_drift_detected = false;
        self.verification_receipts = summary.iter().map(|line| bounded(line, 1000)).collect();
        if passed {
            for failure in &mut self.failures {
                failure.resolved = true;
            }
            self.remaining_blockers
                .retain(|item| !item.starts_with("verification failed"));
        } else {
            push_unique(
                &mut self.remaining_blockers,
                "verification failed; authoritative repair remains required".to_owned(),
                64,
            );
        }
        for line in summary {
            if !passed || looks_like_failure(line) {
                self.record_failure(line);
            }
        }
        collect_evidence(session, self);
        Ok(())
    }

    fn record_failure(&mut self, summary: &str) {
        let summary = bounded(summary, 1200);
        let fingerprint = hash_bytes(summary.as_bytes());
        if let Some(existing) = self
            .failures
            .iter_mut()
            .find(|item| item.fingerprint == fingerprint)
        {
            existing.occurrences = existing.occurrences.saturating_add(1);
            existing.resolved = false;
            return;
        }
        self.failures.push(TrajectoryFailure {
            fingerprint,
            summary,
            occurrences: 1,
            resolved: false,
        });
        if self.failures.len() > MAX_FAILURES {
            self.failures.drain(..self.failures.len() - MAX_FAILURES);
        }
    }

    fn render(&self) -> MedusaResult<String> {
        let json = serde_json::to_string(self).map_err(json_error)?;
        Ok(format!(
            "[medusa-coding-trajectory-v1]\nAUTHORITATIVE DURABLE CODING TRAJECTORY. Preserve objective/constraints, do not rediscover resolved evidence, do not reuse stale verification after repository drift, and do not retry disproved hypotheses without new evidence.\n{}",
            bounded(&json, MAX_RENDER_CHARS)
        ))
    }
}

pub(crate) fn refresh_and_render(session: &AgentSession) -> MedusaResult<String> {
    let mut state = load(session)?.unwrap_or(CodingTrajectoryState::new(session)?);
    state.synchronize(session)?;
    persist(session, &state)?;
    state.render()
}

pub(crate) fn record_mutation(session: &AgentSession, paths: &[String]) -> MedusaResult<()> {
    let mut state = load(session)?.unwrap_or(CodingTrajectoryState::new(session)?);
    state.observe_mutation(session, paths)?;
    persist(session, &state)
}

pub(crate) fn record_verification(
    session: &AgentSession,
    summary: &[String],
    passed: bool,
) -> MedusaResult<()> {
    let mut state = load(session)?.unwrap_or(CodingTrajectoryState::new(session)?);
    state.observe_verification(session, summary, passed)?;
    persist(session, &state)
}

pub(crate) fn snapshot(session: &AgentSession) -> MedusaResult<serde_json::Value> {
    let mut state = load(session)?.unwrap_or(CodingTrajectoryState::new(session)?);
    state.synchronize(session)?;
    persist(session, &state)?;
    serde_json::to_value(state).map_err(json_error)
}

fn collect_user_constraints(session: &AgentSession, target: &mut Vec<String>) {
    for message in &session.messages {
        if message.role != Role::User {
            continue;
        }
        for block in &message.content {
            if let MessageBlock::Text { text } = block {
                let text = text.trim();
                if text.is_empty()
                    || text.starts_with("[medusa-compaction-v1]")
                    || text.starts_with("[medusa-compaction-v2]")
                    || text.starts_with("[medusa-coding-trajectory-v1]")
                {
                    continue;
                }
                push_unique(target, bounded(text, 2000), MAX_CONSTRAINTS);
            }
        }
    }
}

fn collect_evidence(session: &AgentSession, state: &mut CodingTrajectoryState) {
    for line in &session.evidence {
        state.evidence_refs.insert(hash_bytes(line.as_bytes()));
        if looks_like_failure(line) {
            state.record_failure(line);
        }
    }
    while state.evidence_refs.len() > MAX_EVIDENCE_REFS {
        if let Some(first) = state.evidence_refs.iter().next().cloned() {
            state.evidence_refs.remove(&first);
        } else {
            break;
        }
    }
}

fn looks_like_failure(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("failed")
        || lower.contains("failure")
        || lower.contains("error")
        || lower.contains("panic")
        || lower.contains("unresolved")
}

fn repository_identity(repo: &Path) -> MedusaResult<TrajectoryRepositoryIdentity> {
    let head = git(repo, &["rev-parse", "HEAD"]);
    let workspace = git(repo, &["status", "--porcelain=v1", "--untracked-files=all"])
        .unwrap_or_else(|| fallback_workspace(repo));
    Ok(TrajectoryRepositoryIdentity {
        head,
        workspace_fingerprint: hash_bytes(workspace.as_bytes()),
    })
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

fn fallback_workspace(repo: &Path) -> String {
    fs::metadata(repo)
        .map(|meta| format!("{}:{}", repo.display(), meta.len()))
        .unwrap_or_else(|_| repo.display().to_string())
}

fn state_path(session: &AgentSession) -> PathBuf {
    session
        .repo
        .join(".medusa")
        .join("coding-trajectories")
        .join(format!("{}.json", session.id))
}

fn load(session: &AgentSession) -> MedusaResult<Option<CodingTrajectoryState>> {
    let path = state_path(session);
    if !path.is_file() {
        return Ok(None);
    }
    let state: CodingTrajectoryState = serde_json::from_slice(&fs::read(path)?)?;
    Ok(Some(state))
}

fn persist(session: &AgentSession, state: &CodingTrajectoryState) -> MedusaResult<()> {
    let path = state_path(session);
    let parent = path
        .parent()
        .ok_or_else(|| invalid("coding trajectory path has no parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension("json.tmp");
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(state).map_err(json_error)?,
    )?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn push_unique(target: &mut Vec<String>, value: String, limit: usize) {
    if !target.contains(&value) {
        target.push(value);
    }
    if target.len() > limit {
        target.drain(..target.len() - limit);
    }
}

fn bounded(value: &str, max_chars: usize) -> String {
    let mut output = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars {
        output.push('…');
    }
    output
}

fn hash_bytes(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message.into(),
    )
}

fn json_error(error: serde_json::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        error.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use medusa_core::SessionId;
    use medusa_provider::{Message, MessageBlock};
    use time::OffsetDateTime;

    fn session(repo: &Path) -> AgentSession {
        AgentSession {
            id: SessionId::new(),
            objective: "fix the bug without changing the public API".to_owned(),
            repo: repo.to_path_buf(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            completed: false,
            turn: 0,
            plan: vec![crate::session::AgentPlanStep {
                title: "repair implementation".to_owned(),
                status: AgentPlanStepStatus::InProgress,
            }],
            pending_question: None,
            messages: vec![Message {
                role: Role::User,
                content: vec![MessageBlock::Text {
                    text: "keep compatibility and run the regression test".to_owned(),
                }],
            }],
            events: Vec::new(),
            evidence: Vec::new(),
            tool_artifacts: Vec::new(),
            world_model: None,
            approval_grants: Vec::new(),
            approval_receipts: Vec::new(),
            rollback_receipts: Vec::new(),
        }
    }

    #[test]
    fn state_survives_reload_with_constraints_failures_and_verification() {
        let repo = tempfile::tempdir().expect("repo");
        let mut session = session(repo.path());
        record_mutation(&session, &["src/lib.rs".to_owned()]).expect("mutation");
        session
            .evidence
            .push("test failure: expected 42".to_owned());
        record_verification(&session, &["unit-tests=failed".to_owned()], false)
            .expect("verification");
        let rendered = refresh_and_render(&session).expect("render");
        assert!(rendered.contains("keep compatibility"));
        assert!(rendered.contains("src/lib.rs"));
        assert!(rendered.contains("unit-tests=failed"));
        let loaded = load(&session).expect("load").expect("state");
        assert_eq!(loaded.objective, session.objective);
        assert_eq!(loaded.mutation_generation, 1);
        assert_eq!(loaded.failures.len(), 2);
    }

    #[test]
    fn duplicate_failure_is_deduplicated_with_occurrence_count() {
        let repo = tempfile::tempdir().expect("repo");
        let session = session(repo.path());
        record_verification(&session, &["compile error E0308".to_owned()], false).expect("first");
        record_verification(&session, &["compile error E0308".to_owned()], false).expect("second");
        let loaded = load(&session).expect("load").expect("state");
        let failure = loaded
            .failures
            .iter()
            .find(|item| item.summary == "compile error E0308")
            .expect("failure");
        assert_eq!(failure.occurrences, 2);
    }

    #[test]
    fn snapshot_is_canonical_and_bound_to_session() {
        let repo = tempfile::tempdir().expect("repo");
        let session = session(repo.path());
        let first = snapshot(&session).expect("snapshot");
        let second = snapshot(&session).expect("snapshot again");
        assert_eq!(first, second);
        assert_eq!(first["session_id"], session.id.as_str());
    }
}
