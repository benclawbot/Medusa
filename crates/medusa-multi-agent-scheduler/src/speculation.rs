//! Typed, durable policy and accounting for bounded speculative implementation.
//!
//! Speculation is never integration authority. This module only describes whether a task may
//! prepare reversible work, records the exact assumptions and budget under which it ran, and
//! provides the fail-closed promotion decision consumed by the production runtime.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{ExecutionLane, PlanningResult, RiskLevel, ScopeResolution, TaskKind};

const SCHEMA_VERSION: u16 = 1;
const HISTORY_SCHEMA_VERSION: u16 = 1;
const ADAPTIVE_MIN_SAMPLES: u64 = 4;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeculationTaskClass {
    MediumRiskResolvedMutation,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SpeculationBudget {
    pub max_concurrent_tasks: u16,
    pub max_model_turns: u32,
    pub max_attempts: u32,
    pub max_wall_time_ms: u64,
    pub max_compute_units: u64,
    pub max_waste_ms: u64,
}

impl Default for SpeculationBudget {
    fn default() -> Self {
        Self {
            max_concurrent_tasks: 1,
            max_model_turns: 4,
            max_attempts: 1,
            max_wall_time_ms: 120_000,
            max_compute_units: 8,
            max_waste_ms: 120_000,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SpeculationAssumptions {
    pub plan_fingerprint: String,
    pub task_id: String,
    pub task_context_fingerprint: String,
    pub repository_scope: Vec<String>,
    pub affected_components: Vec<String>,
    pub promotion_dependencies: Vec<String>,
    pub confidence_milli: u16,
    pub risk: RiskLevel,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SpeculationPolicy {
    pub eligible: bool,
    pub class: Option<SpeculationTaskClass>,
    pub rationale: String,
    pub budget: SpeculationBudget,
    pub assumptions: Option<SpeculationAssumptions>,
}

/// Selects speculation only for the deliberately narrow production lane from #690.
///
/// Fast mutation already avoids model preflight and high-risk/full-orchestration work is
/// intentionally ineligible. Medium-risk resolved mutations may prepare one provisional
/// implementer when confidence is high and the write scope is exact.
#[must_use]
pub fn policy_for(planning: &PlanningResult) -> SpeculationPolicy {
    let budget = SpeculationBudget::default();
    let implementation = planning.task(TaskKind::Implementation);
    let eligible = planning.lane == ExecutionLane::StandardMutation
        && planning.risk == RiskLevel::Medium
        && planning.confidence_milli >= 850
        && planning.scope.resolution == ScopeResolution::Resolved
        && !planning.scope.effective.is_empty()
        && !planning.scope.effective.iter().any(|path| path == "repository")
        && implementation.is_some_and(|planned| planned.task.speculative);
    if !eligible {
        return SpeculationPolicy {
            eligible: false,
            class: None,
            rationale: "speculation requires a high-confidence medium-risk mutation with exact resolved scope"
                .to_owned(),
            budget,
            assumptions: None,
        };
    }
    let implementation = implementation.expect("eligibility requires implementation task");
    let mut scope = planning.scope.effective.clone();
    scope.sort();
    scope.dedup();
    let mut components = planning.affected_components.clone();
    components.sort();
    components.dedup();
    let mut dependencies = implementation.task.dependencies.clone();
    dependencies.sort();
    dependencies.dedup();
    let mut assumptions = SpeculationAssumptions {
        plan_fingerprint: planning.fingerprint.clone(),
        task_id: implementation.task.id.clone(),
        task_context_fingerprint: implementation.context_fingerprint.clone(),
        repository_scope: scope,
        affected_components: components,
        promotion_dependencies: dependencies,
        confidence_milli: planning.confidence_milli,
        risk: planning.risk,
        fingerprint: String::new(),
    };
    assumptions.fingerprint = hash(&(
        &assumptions.plan_fingerprint,
        &assumptions.task_id,
        &assumptions.task_context_fingerprint,
        &assumptions.repository_scope,
        &assumptions.affected_components,
        &assumptions.promotion_dependencies,
        assumptions.confidence_milli,
        assumptions.risk,
    ));
    SpeculationPolicy {
        eligible: true,
        class: Some(SpeculationTaskClass::MediumRiskResolvedMutation),
        rationale: "resolved medium-risk scope may prepare one disposable implementer while promotion dependencies continue"
            .to_owned(),
        budget,
        assumptions: Some(assumptions),
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InvalidationReason {
    ScopeChanged,
    RepositoryDrift,
    PolicyChanged,
    RiskEscalated,
    ConflictingEvidence,
    StaleGraph,
    CapabilityUnavailable,
    UserSteering,
    Cancellation,
    OverlappingAcceptedMutation,
    BudgetExceeded,
    CrashRecovery,
    PromotionMismatch,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeculationState {
    Proposed,
    Running,
    Prepared { candidate_fingerprint: String },
    Promoted { candidate_fingerprint: String },
    Invalidated { reason: InvalidationReason, detail: String },
    Discarded { detail: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SpeculationRecord {
    schema_version: u16,
    pub class: SpeculationTaskClass,
    pub assumptions: SpeculationAssumptions,
    pub repository_fingerprint: String,
    pub budget: SpeculationBudget,
    pub state: SpeculationState,
    pub model_turns: u32,
    pub attempts: u32,
    pub compute_units: u64,
    pub elapsed_ms: u64,
    pub retained_useful_ms: u64,
    pub wasted_ms: u64,
    pub authoritative_candidate: Option<String>,
    revision: u64,
    fingerprint: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromotionCheck {
    pub plan_fingerprint: String,
    pub repository_fingerprint: String,
    pub repository_scope: Vec<String>,
    pub dependency_ids: Vec<String>,
    pub task_context_fingerprint: String,
    pub candidate_fingerprint: String,
}

pub struct SpeculationLedger {
    path: PathBuf,
    record: SpeculationRecord,
}

impl SpeculationLedger {
    pub fn open_or_create(
        path: impl Into<PathBuf>,
        policy: &SpeculationPolicy,
        repository_fingerprint: impl Into<String>,
    ) -> Result<Self, String> {
        if !policy.eligible {
            return Err("ineligible work cannot create a speculation ledger".to_owned());
        }
        let repository_fingerprint = repository_fingerprint.into();
        let class = policy
            .class
            .ok_or_else(|| "eligible speculation policy has no task class".to_owned())?;
        let assumptions = policy
            .assumptions
            .clone()
            .ok_or_else(|| "eligible speculation policy has no assumptions".to_owned())?;
        let path = path.into();
        if path.is_file() {
            let record: SpeculationRecord = serde_json::from_slice(
                &fs::read(&path).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            let ledger = Self { path, record };
            ledger.validate()?;
            if ledger.record.class != class
                || ledger.record.assumptions != assumptions
                || ledger.record.repository_fingerprint != repository_fingerprint
            {
                return Err("speculation ledger belongs to different assumptions".to_owned());
            }
            return Ok(ledger);
        }
        let mut ledger = Self {
            path,
            record: SpeculationRecord {
                schema_version: SCHEMA_VERSION,
                class,
                assumptions,
                repository_fingerprint,
                budget: policy.budget,
                state: SpeculationState::Proposed,
                model_turns: 0,
                attempts: 0,
                compute_units: 0,
                elapsed_ms: 0,
                retained_useful_ms: 0,
                wasted_ms: 0,
                authoritative_candidate: None,
                revision: 0,
                fingerprint: String::new(),
            },
        };
        ledger.refresh();
        ledger.persist()?;
        Ok(ledger)
    }

    pub fn load(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let record: SpeculationRecord = serde_json::from_slice(
            &fs::read(&path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let ledger = Self { path, record };
        ledger.validate()?;
        Ok(ledger)
    }

    pub fn begin(&mut self) -> Result<(), String> {
        match self.record.state {
            SpeculationState::Proposed => {}
            SpeculationState::Running => return Ok(()),
            _ => return Err("terminal or prepared speculation cannot be restarted".to_owned()),
        }
        if self.record.budget.max_concurrent_tasks == 0 || self.record.budget.max_attempts == 0 {
            return Err("speculation budget forbids dispatch".to_owned());
        }
        self.record.attempts = self.record.attempts.saturating_add(1);
        if self.record.attempts > self.record.budget.max_attempts {
            return self.invalidate(
                InvalidationReason::BudgetExceeded,
                "speculative attempt budget exceeded",
            );
        }
        self.record.state = SpeculationState::Running;
        self.commit()
    }

    pub fn account(
        &mut self,
        model_turns: u32,
        compute_units: u64,
        elapsed_ms: u64,
    ) -> Result<(), String> {
        self.record.model_turns = self.record.model_turns.saturating_add(model_turns);
        self.record.compute_units = self.record.compute_units.saturating_add(compute_units);
        self.record.elapsed_ms = self.record.elapsed_ms.saturating_add(elapsed_ms);
        if self.record.model_turns > self.record.budget.max_model_turns
            || self.record.compute_units > self.record.budget.max_compute_units
            || self.record.elapsed_ms > self.record.budget.max_wall_time_ms
        {
            self.record.wasted_ms = self.record.elapsed_ms;
            self.record.state = SpeculationState::Invalidated {
                reason: InvalidationReason::BudgetExceeded,
                detail: "speculative resource budget exceeded".to_owned(),
            };
        }
        self.commit()
    }

    pub fn prepared(&mut self, candidate_fingerprint: impl Into<String>) -> Result<(), String> {
        let candidate_fingerprint = candidate_fingerprint.into();
        if candidate_fingerprint.trim().is_empty() {
            return Err("prepared speculation requires a candidate fingerprint".to_owned());
        }
        if !matches!(self.record.state, SpeculationState::Running) {
            return Err("only running speculation can become prepared".to_owned());
        }
        self.record.state = SpeculationState::Prepared {
            candidate_fingerprint,
        };
        self.commit()
    }

    pub fn promotion_decision(&self, check: &PromotionCheck) -> Result<(), InvalidationReason> {
        let SpeculationState::Prepared {
            candidate_fingerprint,
        } = &self.record.state
        else {
            return Err(InvalidationReason::PromotionMismatch);
        };
        let normalized_scope = normalized(check.repository_scope.clone());
        let normalized_dependencies = normalized(check.dependency_ids.clone());
        if check.plan_fingerprint != self.record.assumptions.plan_fingerprint
            || check.task_context_fingerprint != self.record.assumptions.task_context_fingerprint
        {
            return Err(InvalidationReason::PolicyChanged);
        }
        if check.repository_fingerprint != self.record.repository_fingerprint {
            return Err(InvalidationReason::RepositoryDrift);
        }
        if normalized_scope != self.record.assumptions.repository_scope {
            return Err(InvalidationReason::ScopeChanged);
        }
        if normalized_dependencies != self.record.assumptions.promotion_dependencies {
            return Err(InvalidationReason::ConflictingEvidence);
        }
        if &check.candidate_fingerprint != candidate_fingerprint {
            return Err(InvalidationReason::PromotionMismatch);
        }
        if let Some(authoritative) = &self.record.authoritative_candidate
            && authoritative != candidate_fingerprint
        {
            return Err(InvalidationReason::OverlappingAcceptedMutation);
        }
        Ok(())
    }

    pub fn promote(&mut self, check: &PromotionCheck, saved_ms: u64) -> Result<(), String> {
        self.promotion_decision(check)
            .map_err(|reason| format!("speculative promotion rejected: {reason:?}"))?;
        if let Some(authoritative) = &self.record.authoritative_candidate
            && authoritative != &check.candidate_fingerprint
        {
            return Err("a different speculative candidate is already authoritative".to_owned());
        }
        self.record.authoritative_candidate = Some(check.candidate_fingerprint.clone());
        self.record.retained_useful_ms = saved_ms;
        self.record.state = SpeculationState::Promoted {
            candidate_fingerprint: check.candidate_fingerprint.clone(),
        };
        self.commit()
    }

    pub fn invalidate(
        &mut self,
        reason: InvalidationReason,
        detail: impl Into<String>,
    ) -> Result<(), String> {
        let detail = detail.into();
        self.record.wasted_ms = self.record.elapsed_ms;
        self.record.state = SpeculationState::Invalidated { reason, detail };
        self.commit()
    }

    pub fn discard(&mut self, detail: impl Into<String>) -> Result<(), String> {
        self.record.wasted_ms = self.record.elapsed_ms;
        self.record.state = SpeculationState::Discarded {
            detail: detail.into(),
        };
        self.commit()
    }

    pub fn recover_interrupted(&mut self) -> Result<bool, String> {
        if matches!(self.record.state, SpeculationState::Running) {
            self.record.wasted_ms = self.record.elapsed_ms;
            self.record.state = SpeculationState::Invalidated {
                reason: InvalidationReason::CrashRecovery,
                detail: "running speculation recovered without promotion authority".to_owned(),
            };
            self.commit()?;
            return Ok(true);
        }
        Ok(false)
    }

    #[must_use]
    pub fn record(&self) -> &SpeculationRecord {
        &self.record
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.record.schema_version != SCHEMA_VERSION
            || self.record.assumptions.fingerprint
                != hash(&(
                    &self.record.assumptions.plan_fingerprint,
                    &self.record.assumptions.task_id,
                    &self.record.assumptions.task_context_fingerprint,
                    &self.record.assumptions.repository_scope,
                    &self.record.assumptions.affected_components,
                    &self.record.assumptions.promotion_dependencies,
                    self.record.assumptions.confidence_milli,
                    self.record.assumptions.risk,
                ))
            || self.record.fingerprint != record_fingerprint(&self.record)
        {
            return Err("speculation ledger is incomplete or corrupted".to_owned());
        }
        Ok(())
    }

    fn commit(&mut self) -> Result<(), String> {
        self.record.revision = self.record.revision.saturating_add(1);
        self.refresh();
        self.persist()
    }

    fn refresh(&mut self) {
        self.record.fingerprint = record_fingerprint(&self.record);
    }

    fn persist(&self) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "speculation ledger path has no parent".to_owned())?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let temporary = self.path.with_extension("json.tmp");
        fs::write(
            &temporary,
            serde_json::to_vec_pretty(&self.record).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        #[cfg(windows)]
        if self.path.exists() {
            fs::remove_file(&self.path).map_err(|error| error.to_string())?;
        }
        fs::rename(temporary, &self.path).map_err(|error| error.to_string())
    }
}

fn record_fingerprint(record: &SpeculationRecord) -> String {
    hash(&(
        record.schema_version,
        record.class,
        &record.assumptions,
        &record.repository_fingerprint,
        record.budget,
        &record.state,
        record.model_turns,
        record.attempts,
        record.compute_units,
        record.elapsed_ms,
        record.retained_useful_ms,
        record.wasted_ms,
        &record.authoritative_candidate,
        record.revision,
    ))
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SpeculationHistory {
    schema_version: u16,
    pub samples: u64,
    pub promoted: u64,
    pub invalidated: u64,
    pub discarded: u64,
    pub retained_useful_ms: u64,
    pub wasted_ms: u64,
}

impl SpeculationHistory {
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.is_file() {
            return Ok(Self {
                schema_version: HISTORY_SCHEMA_VERSION,
                ..Self::default()
            });
        }
        let history: Self = serde_json::from_slice(
            &fs::read(path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        if history.schema_version != HISTORY_SCHEMA_VERSION {
            return Err("unsupported speculation history schema".to_owned());
        }
        Ok(history)
    }

    #[must_use]
    pub fn allows_speculation(&self) -> bool {
        self.samples < ADAPTIVE_MIN_SAMPLES || self.retained_useful_ms >= self.wasted_ms
    }

    pub fn observe(&mut self, record: &SpeculationRecord) {
        self.samples = self.samples.saturating_add(1);
        self.retained_useful_ms = self
            .retained_useful_ms
            .saturating_add(record.retained_useful_ms);
        self.wasted_ms = self.wasted_ms.saturating_add(record.wasted_ms);
        match record.state {
            SpeculationState::Promoted { .. } => self.promoted = self.promoted.saturating_add(1),
            SpeculationState::Invalidated { .. } => {
                self.invalidated = self.invalidated.saturating_add(1)
            }
            SpeculationState::Discarded { .. } => {
                self.discarded = self.discarded.saturating_add(1)
            }
            _ => {}
        }
    }

    pub fn persist(&self, path: &Path) -> Result<(), String> {
        let parent = path
            .parent()
            .ok_or_else(|| "speculation history path has no parent".to_owned())?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        fs::write(
            path,
            serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())
    }
}

fn normalized(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .filter_map(|value| {
            let value = value.trim().replace('\\', "/");
            (!value.is_empty()).then_some(value)
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn hash<T: Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PlannerInput, plan_typed};

    fn medium_plan() -> PlanningResult {
        plan_typed(PlannerInput {
            objective: "Update src/a.rs and src/b.rs".to_owned(),
            attachment_count: 0,
            repository_paths: vec!["src/a.rs".to_owned(), "src/b.rs".to_owned()],
        })
        .expect("plan")
    }

    #[test]
    fn only_high_confidence_medium_resolved_mutation_is_eligible() {
        let plan = medium_plan();
        assert_eq!(plan.lane, ExecutionLane::StandardMutation);
        let policy = policy_for(&plan);
        assert!(policy.eligible);
        assert_eq!(policy.budget.max_concurrent_tasks, 1);

        let fast = plan_typed(PlannerInput {
            objective: "Update src/a.rs".to_owned(),
            attachment_count: 0,
            repository_paths: vec!["src/a.rs".to_owned()],
        })
        .expect("fast plan");
        assert!(!policy_for(&fast).eligible);
    }

    #[test]
    fn promotion_fails_closed_on_dependency_scope_or_repository_drift() {
        let plan = medium_plan();
        let policy = policy_for(&plan);
        let directory = tempfile::tempdir().expect("tempdir");
        let mut ledger = SpeculationLedger::open_or_create(
            directory.path().join("speculation.json"),
            &policy,
            "repo-a",
        )
        .expect("ledger");
        ledger.begin().expect("begin");
        ledger.prepared("candidate-a").expect("prepared");
        let assumptions = policy.assumptions.expect("assumptions");
        let valid = PromotionCheck {
            plan_fingerprint: assumptions.plan_fingerprint.clone(),
            repository_fingerprint: "repo-a".to_owned(),
            repository_scope: assumptions.repository_scope.clone(),
            dependency_ids: assumptions.promotion_dependencies.clone(),
            task_context_fingerprint: assumptions.task_context_fingerprint.clone(),
            candidate_fingerprint: "candidate-a".to_owned(),
        };
        assert_eq!(ledger.promotion_decision(&valid), Ok(()));
        let mut drift = valid.clone();
        drift.repository_fingerprint = "repo-b".to_owned();
        assert_eq!(
            ledger.promotion_decision(&drift),
            Err(InvalidationReason::RepositoryDrift)
        );
        let mut scope = valid.clone();
        scope.repository_scope.push("src/c.rs".to_owned());
        assert_eq!(
            ledger.promotion_decision(&scope),
            Err(InvalidationReason::ScopeChanged)
        );
        let mut dependency = valid;
        dependency.dependency_ids.clear();
        assert_eq!(
            ledger.promotion_decision(&dependency),
            Err(InvalidationReason::ConflictingEvidence)
        );
    }

    #[test]
    fn waste_budget_and_crash_recovery_never_promote() {
        let plan = medium_plan();
        let policy = policy_for(&plan);
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("speculation.json");
        let mut ledger =
            SpeculationLedger::open_or_create(&path, &policy, "repo").expect("ledger");
        ledger.begin().expect("begin");
        ledger
            .account(policy.budget.max_model_turns + 1, 1, 1)
            .expect("account");
        assert!(matches!(
            ledger.record().state,
            SpeculationState::Invalidated {
                reason: InvalidationReason::BudgetExceeded,
                ..
            }
        ));

        let path2 = directory.path().join("running.json");
        let mut running =
            SpeculationLedger::open_or_create(&path2, &policy, "repo").expect("running ledger");
        running.begin().expect("begin running");
        drop(running);
        let mut restored = SpeculationLedger::load(path2).expect("restore");
        assert!(restored.recover_interrupted().expect("recover"));
        assert!(matches!(
            restored.record().state,
            SpeculationState::Invalidated {
                reason: InvalidationReason::CrashRecovery,
                ..
            }
        ));
    }

    #[test]
    fn adaptive_history_disables_negative_value_speculation() {
        let mut history = SpeculationHistory {
            schema_version: HISTORY_SCHEMA_VERSION,
            ..SpeculationHistory::default()
        };
        history.samples = ADAPTIVE_MIN_SAMPLES;
        history.retained_useful_ms = 100;
        history.wasted_ms = 101;
        assert!(!history.allows_speculation());
        history.retained_useful_ms = 101;
        assert!(history.allows_speculation());
    }
}
