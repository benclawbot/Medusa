use std::{collections::BTreeSet, path::PathBuf};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use ulid::Ulid;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperienceScope {
    Global,
    Language,
    Repository,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningAction {
    Ignore,
    Remember,
    CreateSkill,
    PatchSkill,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerificationEvidence {
    pub command: String,
    pub passed: bool,
    pub summary: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExperienceRecord {
    pub id: String,
    pub session_id: String,
    pub parent_session_id: Option<String>,
    pub created_at: String,
    pub goal: String,
    pub repository_fingerprint: String,
    pub successful_procedure: Vec<String>,
    pub failed_approaches: Vec<String>,
    pub user_corrections: Vec<String>,
    pub commands_and_tools: Vec<String>,
    pub files_and_symbols: BTreeSet<String>,
    pub verification: Vec<VerificationEvidence>,
    pub environmental_assumptions: Vec<String>,
    pub reusable: bool,
    pub suggested_scope: ExperienceScope,
    pub confidence_milli: u16,
    pub provenance_digest: String,
}

impl ExperienceRecord {
    pub fn new(session_id: impl Into<String>, goal: impl Into<String>) -> MedusaResult<Self> {
        let session_id = session_id.into();
        let goal = goal.into();
        if session_id.trim().is_empty() || goal.trim().is_empty() {
            return Err(invalid("experience requires a session id and goal"));
        }
        Ok(Self {
            id: Ulid::new().to_string(),
            session_id,
            parent_session_id: None,
            created_at: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .map_err(|error| invalid(format!("cannot format experience timestamp: {error}")))?,
            goal,
            repository_fingerprint: String::new(),
            successful_procedure: Vec::new(),
            failed_approaches: Vec::new(),
            user_corrections: Vec::new(),
            commands_and_tools: Vec::new(),
            files_and_symbols: BTreeSet::new(),
            verification: Vec::new(),
            environmental_assumptions: Vec::new(),
            reusable: false,
            suggested_scope: ExperienceScope::Repository,
            confidence_milli: 0,
            provenance_digest: String::new(),
        })
    }

    pub fn seal(mut self) -> MedusaResult<Self> {
        if self.successful_procedure.is_empty() {
            return Err(invalid("experience has no successful procedure"));
        }
        if !self.verification.iter().any(|evidence| evidence.passed) {
            return Err(invalid("experience has no passing verification evidence"));
        }
        self.confidence_milli = self.confidence_milli.min(1_000);
        self.provenance_digest = digest_json(&self)?;
        Ok(self)
    }

    #[must_use]
    pub fn complexity_score(&self) -> usize {
        self.successful_procedure.len()
            + self.failed_approaches.len().saturating_mul(2)
            + self.user_corrections.len().saturating_mul(3)
            + self.verification.len()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SkillCandidate {
    pub name: String,
    pub summary: String,
    pub instructions: String,
    pub supporting_files: Vec<PathBuf>,
    pub scope: ExperienceScope,
    pub matched_existing_skill: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LearningProposal {
    pub id: String,
    pub action: LearningAction,
    pub experience_id: String,
    pub why_learn: String,
    pub existing_skill: Option<String>,
    pub proposed_patch: String,
    pub evidence: Vec<String>,
    pub scope: ExperienceScope,
    pub confidence_milli: u16,
    pub rollback_revision: Option<String>,
    pub requires_approval: bool,
}

pub trait LearningPolicy: Send + Sync {
    fn decide(
        &self,
        experience: &ExperienceRecord,
        matched_skill: Option<&str>,
    ) -> MedusaResult<LearningAction>;
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuleBasedLearningPolicy {
    pub minimum_complexity: usize,
    pub minimum_confidence_milli: u16,
}

impl Default for RuleBasedLearningPolicy {
    fn default() -> Self {
        Self {
            minimum_complexity: 5,
            minimum_confidence_milli: 700,
        }
    }
}

impl LearningPolicy for RuleBasedLearningPolicy {
    fn decide(
        &self,
        experience: &ExperienceRecord,
        matched_skill: Option<&str>,
    ) -> MedusaResult<LearningAction> {
        if experience.provenance_digest.is_empty() {
            return Err(invalid("experience must be sealed before learning"));
        }
        if experience.confidence_milli < self.minimum_confidence_milli {
            return Ok(LearningAction::Remember);
        }
        if !experience.reusable || experience.complexity_score() < self.minimum_complexity {
            return Ok(LearningAction::Ignore);
        }
        Ok(if matched_skill.is_some() {
            LearningAction::PatchSkill
        } else {
            LearningAction::CreateSkill
        })
    }
}

pub struct ExperienceCompiler<P> {
    policy: P,
}

impl<P: LearningPolicy> ExperienceCompiler<P> {
    #[must_use]
    pub fn new(policy: P) -> Self {
        Self { policy }
    }

    pub fn propose(
        &self,
        experience: &ExperienceRecord,
        candidate: &SkillCandidate,
    ) -> MedusaResult<LearningProposal> {
        let action = self
            .policy
            .decide(experience, candidate.matched_existing_skill.as_deref())?;
        let mut evidence = experience
            .verification
            .iter()
            .filter(|item| item.passed)
            .map(|item| format!("{}: {}", item.command, item.summary))
            .collect::<Vec<_>>();
        evidence.extend(experience.user_corrections.iter().cloned());
        Ok(LearningProposal {
            id: Ulid::new().to_string(),
            action,
            experience_id: experience.id.clone(),
            why_learn: format!(
                "session {} produced a reusable procedure with complexity {}",
                experience.session_id,
                experience.complexity_score()
            ),
            existing_skill: candidate.matched_existing_skill.clone(),
            proposed_patch: candidate.instructions.clone(),
            evidence,
            scope: candidate.scope,
            confidence_milli: experience.confidence_milli,
            rollback_revision: candidate.matched_existing_skill.clone(),
            requires_approval: matches!(
                action,
                LearningAction::CreateSkill | LearningAction::PatchSkill
            ),
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CodingContextSummary {
    pub user_goal_and_acceptance_criteria: Vec<String>,
    pub constraints: Vec<String>,
    pub changes_completed: Vec<String>,
    pub changes_pending: Vec<String>,
    pub files_touched_and_why: Vec<String>,
    pub commands_and_exact_results: Vec<String>,
    pub failures_and_rejected_alternatives: Vec<String>,
    pub repository_state: Vec<String>,
    pub uncommitted_changes: Vec<String>,
    pub risks_and_rollback: Vec<String>,
    pub next_action: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionSearchQuery {
    pub query: String,
    pub repository_fingerprint: Option<String>,
    pub tool: Option<String>,
    pub outcome: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionSearchDocument {
    pub session_id: String,
    pub parent_session_id: Option<String>,
    pub repository_fingerprint: String,
    pub text: String,
    pub tools: BTreeSet<String>,
    pub outcome: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionSearchHit {
    pub session_id: String,
    pub parent_session_id: Option<String>,
    pub score: usize,
    pub excerpt: String,
}

#[derive(Clone, Debug, Default)]
pub struct InMemorySessionRecall {
    documents: Vec<SessionSearchDocument>,
}

impl InMemorySessionRecall {
    pub fn insert(&mut self, document: SessionSearchDocument) {
        self.documents.push(document);
    }

    #[must_use]
    pub fn search(&self, query: &SessionSearchQuery) -> Vec<SessionSearchHit> {
        let terms = query
            .query
            .split_whitespace()
            .map(str::to_ascii_lowercase)
            .collect::<Vec<_>>();
        let mut hits = self
            .documents
            .iter()
            .filter(|document| {
                query
                    .repository_fingerprint
                    .as_ref()
                    .is_none_or(|value| &document.repository_fingerprint == value)
                    && query
                        .tool
                        .as_ref()
                        .is_none_or(|value| document.tools.contains(value))
                    && query
                        .outcome
                        .as_ref()
                        .is_none_or(|value| &document.outcome == value)
            })
            .filter_map(|document| {
                let text = document.text.to_ascii_lowercase();
                let score = terms
                    .iter()
                    .filter(|term| text.contains(term.as_str()))
                    .count();
                (score > 0).then_some(SessionSearchHit {
                    session_id: document.session_id.clone(),
                    parent_session_id: document.parent_session_id.clone(),
                    score,
                    excerpt: document.text.chars().take(240).collect(),
                })
            })
            .collect::<Vec<_>>();
        hits.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        hits
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DelegationProfile {
    pub name: String,
    pub tools: BTreeSet<String>,
    pub filesystem: String,
    pub memory: String,
    pub network: String,
    pub secrets: String,
}

impl DelegationProfile {
    pub fn validate(&self) -> MedusaResult<()> {
        if self.name.trim().is_empty() || self.tools.is_empty() {
            return Err(invalid("delegation profile requires a name and tools"));
        }
        Ok(())
    }
}

pub trait MemoryProvider: Send + Sync {
    fn remember(&self, record: &ExperienceRecord) -> MedusaResult<()>;
}
pub trait ContextEngine: Send + Sync {
    fn compress(&self, summary: &CodingContextSummary) -> MedusaResult<String>;
}
pub trait RetrievalProvider: Send + Sync {
    fn search(&self, query: &SessionSearchQuery) -> MedusaResult<Vec<SessionSearchHit>>;
}
pub trait ExecutionBackend: Send + Sync {
    fn execute(&self, profile: &DelegationProfile, task: &str) -> MedusaResult<String>;
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PromptFingerprint {
    pub base_prompt_sha256: String,
    pub skill_catalogue_sha256: String,
    pub project_context_sha256: String,
}

impl PromptFingerprint {
    #[must_use]
    pub fn new(base_prompt: &str, skill_catalogue: &str, project_context: &str) -> Self {
        Self {
            base_prompt_sha256: digest_text(base_prompt),
            skill_catalogue_sha256: digest_text(skill_catalogue),
            project_context_sha256: digest_text(project_context),
        }
    }
}

fn digest_text(value: &str) -> String {
    hex_digest(Sha256::digest(value.as_bytes()))
}
fn digest_json<T: Serialize>(value: &T) -> MedusaResult<String> {
    Ok(hex_digest(Sha256::digest(serde_json::to_vec(value)?)))
}
fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::InvalidInput, ErrorCategory::Validation, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verified_experience() -> ExperienceRecord {
        let mut record = ExperienceRecord::new("session-1", "fix cargo replacement").expect("new");
        record.repository_fingerprint = "repo-a".to_owned();
        record.successful_procedure = vec![
            "stop running process".to_owned(),
            "replace executable from helper".to_owned(),
        ];
        record.failed_approaches = vec!["replace running executable directly".to_owned()];
        record.user_corrections = vec!["preserve package-manager installs".to_owned()];
        record.verification = vec![VerificationEvidence {
            command: "cargo test -p medusa-update".to_owned(),
            passed: true,
            summary: "all update tests passed".to_owned(),
        }];
        record.reusable = true;
        record.confidence_milli = 900;
        record.seal().expect("seal")
    }

    #[test]
    fn compiler_patches_matching_skill_and_requires_approval() {
        let proposal = ExperienceCompiler::new(RuleBasedLearningPolicy::default())
            .propose(
                &verified_experience(),
                &SkillCandidate {
                    name: "windows-update".to_owned(),
                    summary: "Safely update Windows binaries".to_owned(),
                    instructions: "Stop processes, then replace from a helper.".to_owned(),
                    supporting_files: Vec::new(),
                    scope: ExperienceScope::Global,
                    matched_existing_skill: Some("windows-update@3".to_owned()),
                },
            )
            .expect("proposal");
        assert_eq!(proposal.action, LearningAction::PatchSkill);
        assert!(proposal.requires_approval);
        assert_eq!(
            proposal.rollback_revision.as_deref(),
            Some("windows-update@3")
        );
    }

    #[test]
    fn recall_filters_and_ranks_exact_prior_solutions() {
        let mut recall = InMemorySessionRecall::default();
        recall.insert(SessionSearchDocument {
            session_id: "a".to_owned(),
            parent_session_id: None,
            repository_fingerprint: "repo".to_owned(),
            text: "fixed Windows Cargo executable replacement with detached helper".to_owned(),
            tools: BTreeSet::from(["shell".to_owned()]),
            outcome: "success".to_owned(),
        });
        let hits = recall.search(&SessionSearchQuery {
            query: "Windows Cargo replacement".to_owned(),
            repository_fingerprint: Some("repo".to_owned()),
            tool: Some("shell".to_owned()),
            outcome: Some("success".to_owned()),
        });
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].score, 3);
    }

    #[test]
    fn prompt_fingerprint_changes_only_for_changed_layer() {
        let baseline = PromptFingerprint::new("base", "skills", "project");
        let changed = PromptFingerprint::new("base", "skills v2", "project");
        assert_eq!(baseline.base_prompt_sha256, changed.base_prompt_sha256);
        assert_ne!(
            baseline.skill_catalogue_sha256,
            changed.skill_catalogue_sha256
        );
        assert_eq!(
            baseline.project_context_sha256,
            changed.project_context_sha256
        );
    }
}
