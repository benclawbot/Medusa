//! Guarded self-improvement lifecycle with frozen evaluation and automatic rollback.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use ulid::Ulid;

/// One normalized trajectory event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TrajectoryEvent {
    pub kind: String,
    pub success: bool,
    pub detail: String,
}

/// Deterministic trajectory analysis used to propose improvements.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TrajectoryAnalysis {
    pub total_events: usize,
    pub failures: usize,
    pub retries: usize,
    pub verification_failures: usize,
    pub repeated_friction: Vec<String>,
}

#[must_use]
pub fn analyze_trajectory(events: &[TrajectoryEvent]) -> TrajectoryAnalysis {
    let mut counts = BTreeMap::<String, usize>::new();
    let mut failures = 0;
    let mut retries = 0;
    let mut verification_failures = 0;
    for event in events {
        if !event.success {
            failures += 1;
        }
        if event.kind == "retry" {
            retries += 1;
        }
        if event.kind == "verification" && !event.success {
            verification_failures += 1;
        }
        *counts.entry(event.detail.clone()).or_default() += 1;
    }
    let repeated_friction = counts
        .into_iter()
        .filter_map(|(detail, count)| (count > 1).then_some(format!("{detail} ({count}x)")))
        .collect();
    TrajectoryAnalysis {
        total_events: events.len(),
        failures,
        retries,
        verification_failures,
        repeated_friction,
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImprovementTarget {
    Memory,
    Skill,
    Prompt,
    ToolDescription,
    TestDiscovery,
    RecoveryHeuristic,
    RepositoryMap,
    CommandKnowledge,
    ModelRouting,
    ContextRetrieval,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImprovementRisk {
    Low,
    Medium,
    High,
}

/// Reviewable improvement proposal.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImprovementProposal {
    pub id: String,
    pub target: ImprovementTarget,
    pub risk: ImprovementRisk,
    pub source_sessions: Vec<String>,
    pub problem: String,
    pub evidence: Vec<String>,
    pub proposed_change: String,
    pub rejected_alternatives: Vec<String>,
    pub evaluation_plan: String,
    pub safety_analysis: String,
    pub rollback: String,
    pub touched_paths: BTreeSet<PathBuf>,
}

impl ImprovementProposal {
    pub fn validate(&self) -> MedusaResult<()> {
        if self.id.trim().is_empty()
            || self.source_sessions.is_empty()
            || self.problem.trim().is_empty()
            || self.evidence.is_empty()
            || self.proposed_change.trim().is_empty()
            || self.evaluation_plan.trim().is_empty()
            || self.rollback.trim().is_empty()
        {
            return Err(invalid("improvement proposal is incomplete"));
        }
        if self.risk != ImprovementRisk::Low {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                "only low-risk improvements may auto-promote",
            ));
        }
        if matches!(
            self.target,
            ImprovementTarget::ToolDescription
                | ImprovementTarget::ModelRouting
                | ImprovementTarget::RecoveryHeuristic
        ) {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                "executable or core-behavior changes require explicit review",
            ));
        }
        Ok(())
    }
}

/// Evaluation subset provenance.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalSubset {
    Frozen,
    SelfAuthored,
}

/// Deterministic evaluation task with an immutable oracle.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvalTask {
    pub id: String,
    pub category: ImprovementTarget,
    pub subset: EvalSubset,
    pub prompt: String,
    pub required_fragments: Vec<String>,
    pub forbidden_fragments: Vec<String>,
}

/// Frozen corpus manifest and content hash.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvalCorpus {
    pub version: String,
    pub reviewed_by: Option<String>,
    pub frozen_digest: String,
    pub tasks: Vec<EvalTask>,
}

impl EvalCorpus {
    pub fn validate_for_promotion(&self, proposal: &ImprovementProposal) -> MedusaResult<()> {
        if self.reviewed_by.as_deref().is_none_or(str::is_empty) {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                "frozen evaluation subset lacks human review provenance",
            ));
        }
        let frozen = self
            .tasks
            .iter()
            .filter(|task| task.subset == EvalSubset::Frozen)
            .collect::<Vec<_>>();
        if frozen.is_empty() {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                "auto-promotion requires a frozen evaluation subset",
            ));
        }
        if proposal
            .touched_paths
            .iter()
            .any(|path| path.starts_with("eval/frozen"))
        {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                "proposal touched the frozen evaluation subset",
            ));
        }
        if self.frozen_digest != frozen_digest(&frozen)? {
            return Err(MedusaError::new(
                ErrorCode::ChecksumMismatch,
                ErrorCategory::Validation,
                "frozen evaluation digest mismatch",
            ));
        }
        Ok(())
    }
}

/// Versioned low-risk skill behavior used by the deterministic benchmark fixture.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SkillVersion {
    pub name: String,
    pub version: String,
    pub responses: BTreeMap<String, String>,
}

impl SkillVersion {
    fn answer(&self, prompt: &str) -> String {
        self.responses
            .iter()
            .find_map(|(trigger, response)| prompt.contains(trigger).then_some(response.clone()))
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SubsetMetrics {
    pub passed: usize,
    pub total: usize,
}

impl SubsetMetrics {
    #[must_use]
    pub fn score_milli(&self) -> u16 {
        if self.total == 0 {
            0
        } else {
            ((self.passed * 1_000) / self.total) as u16
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BenchmarkResult {
    pub candidate_version: String,
    pub frozen: SubsetMetrics,
    pub self_authored: SubsetMetrics,
    pub task_results: BTreeMap<String, bool>,
}

#[must_use]
pub fn benchmark(candidate: &SkillVersion, corpus: &EvalCorpus) -> BenchmarkResult {
    let mut frozen = SubsetMetrics {
        passed: 0,
        total: 0,
    };
    let mut self_authored = SubsetMetrics {
        passed: 0,
        total: 0,
    };
    let mut task_results = BTreeMap::new();
    for task in &corpus.tasks {
        let answer = candidate.answer(&task.prompt);
        let passed = task
            .required_fragments
            .iter()
            .all(|fragment| answer.contains(fragment))
            && task
                .forbidden_fragments
                .iter()
                .all(|fragment| !answer.contains(fragment));
        let metrics = match task.subset {
            EvalSubset::Frozen => &mut frozen,
            EvalSubset::SelfAuthored => &mut self_authored,
        };
        metrics.total += 1;
        if passed {
            metrics.passed += 1;
        }
        task_results.insert(task.id.clone(), passed);
    }
    BenchmarkResult {
        candidate_version: candidate.version.clone(),
        frozen,
        self_authored,
        task_results,
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PromotionRecord {
    pub proposal_id: String,
    pub skill_name: String,
    pub previous_version: String,
    pub promoted_version: String,
    pub promoted_at: String,
    pub frozen_score_milli: u16,
    pub self_authored_score_milli: u16,
    pub frozen_subset_digest: String,
    pub rollback_bundle: PathBuf,
    pub reverted_at: Option<String>,
    pub revert_reason: Option<String>,
}

pub struct ImprovementStore {
    root: PathBuf,
}

impl ImprovementStore {
    pub fn new(root: impl Into<PathBuf>) -> MedusaResult<Self> {
        let root = root.into().join(".medusa/improvements");
        fs::create_dir_all(root.join("skills"))?;
        fs::create_dir_all(root.join("rollback"))?;
        fs::create_dir_all(root.join("history"))?;
        Ok(Self { root })
    }

    pub fn install_baseline(&self, skill: &SkillVersion) -> MedusaResult<()> {
        let path = self.skill_path(&skill.name);
        if path.exists() {
            return Err(invalid("baseline skill is already installed"));
        }
        atomic_json(&path, skill)
    }

    pub fn active_skill(&self, name: &str) -> MedusaResult<SkillVersion> {
        serde_json::from_slice(&fs::read(self.skill_path(name))?).map_err(Into::into)
    }

    pub fn promote(
        &self,
        proposal: &ImprovementProposal,
        candidate: &SkillVersion,
        corpus: &EvalCorpus,
        allowed_regression_milli: u16,
    ) -> MedusaResult<PromotionRecord> {
        proposal.validate()?;
        corpus.validate_for_promotion(proposal)?;
        if proposal.target != ImprovementTarget::Skill {
            return Err(invalid("this promotion path is restricted to skills"));
        }
        let baseline = self.active_skill(&candidate.name)?;
        let baseline_result = benchmark(&baseline, corpus);
        let candidate_result = benchmark(candidate, corpus);
        let baseline_frozen = baseline_result.frozen.score_milli();
        let candidate_frozen = candidate_result.frozen.score_milli();
        if candidate_frozen.saturating_add(allowed_regression_milli) < baseline_frozen {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                "candidate regressed on the frozen evaluation subset",
            ));
        }
        if candidate_frozen == 0 || candidate_result.frozen.total == 0 {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                "candidate lacks passing frozen-subset evidence",
            ));
        }
        let rollback_bundle =
            self.root
                .join("rollback")
                .join(format!("{}-{}.json", candidate.name, Ulid::new()));
        atomic_json(&rollback_bundle, &baseline)?;
        atomic_json(&self.skill_path(&candidate.name), candidate)?;
        let record = PromotionRecord {
            proposal_id: proposal.id.clone(),
            skill_name: candidate.name.clone(),
            previous_version: baseline.version,
            promoted_version: candidate.version.clone(),
            promoted_at: now()?,
            frozen_score_milli: candidate_frozen,
            self_authored_score_milli: candidate_result.self_authored.score_milli(),
            frozen_subset_digest: corpus.frozen_digest.clone(),
            rollback_bundle,
            reverted_at: None,
            revert_reason: None,
        };
        atomic_json(&self.history_path(&record.proposal_id), &record)?;
        Ok(record)
    }

    pub fn monitor_and_rollback(
        &self,
        record: &mut PromotionRecord,
        corpus: &EvalCorpus,
        regression_floor_milli: u16,
    ) -> MedusaResult<bool> {
        let active = self.active_skill(&record.skill_name)?;
        let result = benchmark(&active, corpus);
        if result.frozen.score_milli() >= regression_floor_milli {
            return Ok(false);
        }
        let previous: SkillVersion = serde_json::from_slice(&fs::read(&record.rollback_bundle)?)?;
        atomic_json(&self.skill_path(&record.skill_name), &previous)?;
        record.reverted_at = Some(now()?);
        record.revert_reason = Some(format!(
            "frozen score {} below floor {regression_floor_milli}",
            result.frozen.score_milli()
        ));
        atomic_json(&self.history_path(&record.proposal_id), record)?;
        Ok(true)
    }

    pub fn replace_active_for_monitoring(&self, skill: &SkillVersion) -> MedusaResult<()> {
        atomic_json(&self.skill_path(&skill.name), skill)
    }

    fn skill_path(&self, name: &str) -> PathBuf {
        self.root.join("skills").join(format!("{name}.json"))
    }

    fn history_path(&self, proposal_id: &str) -> PathBuf {
        self.root
            .join("history")
            .join(format!("{proposal_id}.json"))
    }
}

/// Writes the required per-session learning report.
pub fn write_learning_report(
    path: &Path,
    analysis: &TrajectoryAnalysis,
    proposals: &[ImprovementProposal],
    benchmarks: &[BenchmarkResult],
    promoted: &[PromotionRecord],
) -> MedusaResult<()> {
    let report = format!(
        "# Session Learnings\n\n## What Worked\n- {} successful trajectory events\n\n## What Failed\n- {} failures\n\n## Repeated Friction\n{}\n\n## New Validated Knowledge\n- Benchmark evidence retained with subset provenance.\n\n## Proposed Memory Changes\n- None in this report.\n\n## Proposed Skill Changes\n- {} proposals\n\n## Proposed Harness Changes\n- None auto-promoted.\n\n## Benchmark Results\n- {} benchmark runs\n\n## Promoted Changes\n- {} promotions\n\n## Deferred Changes\n- Medium/high-risk proposals require review.\n",
        analysis.total_events.saturating_sub(analysis.failures),
        analysis.failures,
        if analysis.repeated_friction.is_empty() {
            "- None".to_owned()
        } else {
            analysis
                .repeated_friction
                .iter()
                .map(|item| format!("- {item}"))
                .collect::<Vec<_>>()
                .join("\n")
        },
        proposals.len(),
        benchmarks.len(),
        promoted.len(),
    );
    atomic_bytes(path, report.as_bytes())
}

pub fn frozen_digest(tasks: &[&EvalTask]) -> MedusaResult<String> {
    let mut ordered = tasks.to_vec();
    ordered.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&ordered)?)
    ))
}

fn now() -> MedusaResult<String> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| internal(error.to_string()))
}

fn atomic_json(path: &Path, value: &impl Serialize) -> MedusaResult<()> {
    atomic_bytes(path, &serde_json::to_vec_pretty(value)?)
}

fn atomic_bytes(path: &Path, bytes: &[u8]) -> MedusaResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

fn internal(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus() -> EvalCorpus {
        let tasks = vec![
            EvalTask {
                id: "frozen-rust-workspace-tests".into(),
                category: ImprovementTarget::Skill,
                subset: EvalSubset::Frozen,
                prompt: "diagnose rust workspace ci".into(),
                required_fragments: vec!["cargo test --workspace --all-features".into()],
                forbidden_fragments: vec!["--broken".into()],
            },
            EvalTask {
                id: "self-rust-fmt".into(),
                category: ImprovementTarget::Skill,
                subset: EvalSubset::SelfAuthored,
                prompt: "format rust workspace".into(),
                required_fragments: vec!["cargo fmt --all".into()],
                forbidden_fragments: vec![],
            },
        ];
        let frozen = tasks
            .iter()
            .filter(|task| task.subset == EvalSubset::Frozen)
            .collect::<Vec<_>>();
        EvalCorpus {
            version: "human-reviewed-1".into(),
            reviewed_by: Some("fixture-human-reviewer".into()),
            frozen_digest: frozen_digest(&frozen).expect("digest"),
            tasks,
        }
    }

    fn baseline() -> SkillVersion {
        SkillVersion {
            name: "rust-ci".into(),
            version: "1.0.0".into(),
            responses: BTreeMap::from([
                ("rust workspace ci".into(), "run cargo test".into()),
                ("format rust workspace".into(), "run cargo fmt --all".into()),
            ]),
        }
    }

    fn improved() -> SkillVersion {
        SkillVersion {
            name: "rust-ci".into(),
            version: "1.1.0".into(),
            responses: BTreeMap::from([
                (
                    "rust workspace ci".into(),
                    "run cargo test --workspace --all-features".into(),
                ),
                ("format rust workspace".into(), "run cargo fmt --all".into()),
            ]),
        }
    }

    fn proposal() -> ImprovementProposal {
        ImprovementProposal {
            id: "IMP-20260711-001".into(),
            target: ImprovementTarget::Skill,
            risk: ImprovementRisk::Low,
            source_sessions: vec!["ses-fixture".into()],
            problem: "Rust workspace tests were repeatedly under-scoped.".into(),
            evidence: vec!["artifact://trajectory/retry-1".into()],
            proposed_change: "Use the verified workspace-wide test command.".into(),
            rejected_alternatives: vec!["Change the policy engine.".into()],
            evaluation_plan: "Run frozen and self-authored skill tasks.".into(),
            safety_analysis: "Markdown skill behavior only; no executable change.".into(),
            rollback: "Restore the previous version from the rollback bundle.".into(),
            touched_paths: BTreeSet::from([PathBuf::from(".medusa/skills/rust-ci/SKILL.md")]),
        }
    }

    #[test]
    fn low_risk_skill_is_promoted_then_reverted_on_regression() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = ImprovementStore::new(directory.path()).expect("store");
        store.install_baseline(&baseline()).expect("baseline");
        let corpus = corpus();
        let baseline_result = benchmark(&baseline(), &corpus);
        let candidate_result = benchmark(&improved(), &corpus);
        assert!(candidate_result.frozen.score_milli() > baseline_result.frozen.score_milli());

        let mut record = store
            .promote(&proposal(), &improved(), &corpus, 0)
            .expect("promote");
        assert_eq!(
            store.active_skill("rust-ci").expect("active").version,
            "1.1.0"
        );
        assert_eq!(record.frozen_score_milli, 1_000);

        let regressed = SkillVersion {
            name: "rust-ci".into(),
            version: "1.1.1-regressed".into(),
            responses: BTreeMap::from([(
                "rust workspace ci".into(),
                "run cargo test --broken".into(),
            )]),
        };
        store
            .replace_active_for_monitoring(&regressed)
            .expect("inject regression");
        assert!(
            store
                .monitor_and_rollback(&mut record, &corpus, 1_000)
                .expect("monitor")
        );
        assert_eq!(
            store.active_skill("rust-ci").expect("rolled back").version,
            "1.0.0"
        );
        assert!(record.reverted_at.is_some());
        assert!(record.revert_reason.is_some());
    }

    #[test]
    fn auto_promotion_fails_without_frozen_human_review() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = ImprovementStore::new(directory.path()).expect("store");
        store.install_baseline(&baseline()).expect("baseline");
        let mut corpus = corpus();
        corpus.reviewed_by = None;
        assert!(store.promote(&proposal(), &improved(), &corpus, 0).is_err());
    }

    #[test]
    fn trajectory_analysis_and_learning_report_are_deterministic() {
        let events = vec![
            TrajectoryEvent {
                kind: "retry".into(),
                success: false,
                detail: "workspace test command missing".into(),
            },
            TrajectoryEvent {
                kind: "retry".into(),
                success: false,
                detail: "workspace test command missing".into(),
            },
            TrajectoryEvent {
                kind: "verification".into(),
                success: true,
                detail: "workspace tests passed".into(),
            },
        ];
        let analysis = analyze_trajectory(&events);
        assert_eq!(analysis.failures, 2);
        assert_eq!(analysis.retries, 2);
        assert_eq!(analysis.repeated_friction.len(), 1);
        let directory = tempfile::tempdir().expect("tempdir");
        let report = directory.path().join("LEARNINGS.md");
        write_learning_report(&report, &analysis, &[proposal()], &[], &[]).expect("report");
        let text = fs::read_to_string(report).expect("read report");
        assert!(text.contains("## Proposed Skill Changes"));
        assert!(text.contains("workspace test command missing (2x)"));
    }
}
