//! Deterministic and feedback-driven scheduling for parallel Medusa workers.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use medusa_evidence::{EvidenceBundle, EvidenceDependency};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanningIntent {
    Conversation,
    ReadOnly,
    MutationRequested,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStrategy {
    Direct,
    CoordinatedReadOnly,
    CoordinatedMutation,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionLane {
    Instant,
    FastMutation,
    StandardMutation,
    #[default]
    FullOrchestration,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelTurnBudget {
    pub before_first_edit: u8,
    pub successful_path_total: u8,
    pub repair_attempts: u8,
}

impl Default for ModelTurnBudget {
    fn default() -> Self {
        Self {
            before_first_edit: 3,
            successful_path_total: 8,
            repair_attempts: 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Analysis,
    RiskReview,
    Implementation,
    Review,
    Verification,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeResolution {
    NotRequested,
    Resolved,
    Unresolved,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationAuthority {
    RuntimeController,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RepositoryScope {
    pub requested: Vec<String>,
    pub effective: Vec<String>,
    pub resolution: ScopeResolution,
    pub rationale: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlannedTask {
    pub task: Task,
    pub kind: TaskKind,
    pub title: String,
    pub context_fingerprint: String,
    pub cancellation_authority: CancellationAuthority,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlanningResult {
    pub intent: PlanningIntent,
    pub requested_outcomes: Vec<String>,
    pub affected_components: Vec<String>,
    pub scope: RepositoryScope,
    pub risk: RiskLevel,
    pub confidence_milli: u16,
    pub required_capabilities: Vec<String>,
    pub strategy: ExecutionStrategy,
    #[serde(default)]
    pub lane: ExecutionLane,
    #[serde(default = "default_lane_rationale")]
    pub lane_rationale: String,
    #[serde(default)]
    pub model_turn_budget: ModelTurnBudget,
    pub tasks: Vec<PlannedTask>,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlannerInput {
    pub objective: String,
    pub attachment_count: usize,
    pub repository_paths: Vec<String>,
}

pub fn plan_typed(mut input: PlannerInput) -> Result<PlanningResult, &'static str> {
    input.objective = input.objective.trim().to_owned();
    if input.objective.is_empty() && input.attachment_count == 0 {
        return Err("planning objective cannot be empty");
    }
    input.repository_paths = input
        .repository_paths
        .into_iter()
        .filter_map(|path| normalize_path(&path))
        .collect();
    input.repository_paths.sort();
    input.repository_paths.dedup();

    let lower = input.objective.to_ascii_lowercase();
    let words = lexical_words(&lower);
    let explicitly_read_only = contains_phrase(
        &lower,
        &[
            "without changing",
            "without modifying",
            "do not change",
            "do not modify",
            "don't change",
            "don't modify",
            "read only",
            "read-only",
            "leave unchanged",
            "keep unchanged",
        ],
    );
    let mutation_requested = !explicitly_read_only && contains_mutation_verb(&words);
    let repository_relevant = input.attachment_count > 0
        || words.iter().any(|word| {
            matches!(
                word.as_str(),
                "code"
                    | "codebase"
                    | "crate"
                    | "file"
                    | "repository"
                    | "repo"
                    | "test"
                    | "tests"
                    | "workflow"
            )
        })
        || candidate_paths(&input.objective).next().is_some();
    let conversation = !mutation_requested
        && !repository_relevant
        && input.objective.split_whitespace().count() <= 24;
    let intent = if mutation_requested {
        PlanningIntent::MutationRequested
    } else if conversation {
        PlanningIntent::Conversation
    } else {
        PlanningIntent::ReadOnly
    };

    let requested = candidate_paths(&input.objective).collect::<BTreeSet<_>>();
    let broad_scope = mutation_requested
        && contains_phrase(
            &lower,
            &[
                "repository-wide",
                "repo-wide",
                "whole repository",
                "entire repository",
                "across the repository",
                "all files",
            ],
        );
    let effective = if broad_scope {
        vec!["repository".to_owned()]
    } else {
        requested
            .iter()
            .filter(|candidate| path_exists(candidate, &input.repository_paths))
            .cloned()
            .collect::<Vec<_>>()
    };
    let scope = if !mutation_requested {
        RepositoryScope {
            requested: requested.into_iter().collect(),
            effective: Vec::new(),
            resolution: ScopeResolution::NotRequested,
            rationale: "the accepted intent grants no write authority".to_owned(),
        }
    } else if !effective.is_empty() {
        RepositoryScope {
            requested: requested.into_iter().collect(),
            effective,
            resolution: ScopeResolution::Resolved,
            rationale: if broad_scope {
                "the objective explicitly requested repository-wide mutation".to_owned()
            } else {
                "requested paths were resolved against the repository snapshot".to_owned()
            },
        }
    } else {
        RepositoryScope {
            requested: requested.into_iter().collect(),
            effective: Vec::new(),
            resolution: ScopeResolution::Unresolved,
            rationale: "mutation was requested but no repository write scope was resolved; execution remains read-only"
                .to_owned(),
        }
    };

    let strategy = match (intent, scope.resolution) {
        (PlanningIntent::Conversation, _) => ExecutionStrategy::Direct,
        (PlanningIntent::MutationRequested, ScopeResolution::Resolved) => {
            ExecutionStrategy::CoordinatedMutation
        }
        _ => ExecutionStrategy::CoordinatedReadOnly,
    };
    let high_risk_language = words.iter().any(|word| {
        matches!(
            word.as_str(),
            "architecture"
                | "dependency"
                | "dependencies"
                | "migration"
                | "release"
                | "security"
                | "upgrade"
        )
    });
    let risk = if strategy == ExecutionStrategy::CoordinatedMutation
        && (broad_scope || scope.effective.len() > 2 || high_risk_language)
    {
        RiskLevel::High
    } else if strategy == ExecutionStrategy::CoordinatedMutation && scope.effective.len() == 1 {
        RiskLevel::Low
    } else if strategy == ExecutionStrategy::CoordinatedMutation || repository_relevant {
        RiskLevel::Medium
    } else {
        RiskLevel::Low
    };
    let confidence_milli = match (intent, scope.resolution) {
        (PlanningIntent::MutationRequested, ScopeResolution::Unresolved) => 450,
        (PlanningIntent::MutationRequested, ScopeResolution::Resolved) => 920,
        (PlanningIntent::Conversation, _) => 900,
        _ => 800,
    };
    let affected_components = affected_components(&scope, &input.repository_paths);
    let (lane, lane_rationale) = select_execution_lane(
        strategy,
        risk,
        confidence_milli,
        &scope,
        broad_scope,
        high_risk_language,
    );
    let model_turn_budget = model_turn_budget(lane);
    let tasks = planned_tasks(strategy, &scope);
    let required_capabilities = tasks
        .iter()
        .flat_map(|task| task.task.capabilities.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut result = PlanningResult {
        intent,
        requested_outcomes: vec![input.objective],
        affected_components,
        scope,
        risk,
        confidence_milli,
        required_capabilities,
        strategy,
        lane,
        lane_rationale,
        model_turn_budget,
        tasks,
        fingerprint: String::new(),
    };
    apply_speculation_flags(&mut result);
    result.fingerprint = planning_fingerprint(&result);
    result.validate()?;
    Ok(result)
}

/// Applies fresh revision-bound repository graph evidence to an already fail-closed plan.
///
/// Graph evidence may broaden affected components or raise risk, but stale evidence never narrows
/// scope, lowers risk, or grants mutation authority.
pub fn apply_repository_graph_evidence(
    mut result: PlanningResult,
    affected_components: Vec<String>,
    public_api_risk: bool,
    evidence_current: bool,
) -> Result<PlanningResult, &'static str> {
    if !evidence_current {
        for planned in &mut result.tasks {
            planned.task.speculative = false;
            planned.context_fingerprint = hash(&(
                &planned.task,
                planned.kind,
                &planned.title,
                CancellationAuthority::RuntimeController,
            ));
        }
        result.fingerprint = planning_fingerprint(&result);
        result.validate()?;
        return Ok(result);
    }

    let components = affected_components
        .into_iter()
        .filter_map(|component| {
            let component = component.trim();
            (!component.is_empty()).then(|| component.to_owned())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if !components.is_empty() {
        result.affected_components = components;
    }

    if public_api_risk && result.strategy == ExecutionStrategy::CoordinatedMutation {
        result.risk = RiskLevel::High;
        result.lane = ExecutionLane::FullOrchestration;
        result.lane_rationale =
            "fresh repository graph reports public API risk; full orchestration is required"
                .to_owned();
        result.model_turn_budget = model_turn_budget(ExecutionLane::FullOrchestration);
    }

    apply_speculation_flags(&mut result);
    result.fingerprint = planning_fingerprint(&result);
    result.validate()?;
    Ok(result)
}

impl PlanningResult {
    pub fn validate(&self) -> Result<(), &'static str> {
        let current_fingerprint = self.fingerprint == planning_fingerprint(self);
        let legacy_fingerprint = self.lane == ExecutionLane::FullOrchestration
            && self.lane_rationale == default_lane_rationale()
            && self.model_turn_budget == ModelTurnBudget::default()
            && self.fingerprint == legacy_planning_fingerprint(self);
        if self.confidence_milli > 1_000
            || self.requested_outcomes.is_empty()
            || self
                .requested_outcomes
                .iter()
                .any(|value| value.trim().is_empty())
            || (!current_fingerprint && !legacy_fingerprint)
        {
            return Err("typed planning result is incomplete or corrupted");
        }
        if self.lane_rationale.trim().is_empty()
            || self.model_turn_budget.successful_path_total == 0
            || self.model_turn_budget.before_first_edit
                > self.model_turn_budget.successful_path_total
        {
            return Err("execution lane and model-turn budget are incomplete");
        }
        if self.strategy == ExecutionStrategy::CoordinatedMutation
            && (self.scope.resolution != ScopeResolution::Resolved
                || self.scope.effective.is_empty())
        {
            return Err("mutating execution requires resolved effective scope");
        }
        match self.lane {
            ExecutionLane::Instant if self.strategy != ExecutionStrategy::Direct => {
                return Err("instant execution requires direct strategy");
            }
            ExecutionLane::FastMutation
                if self.strategy != ExecutionStrategy::CoordinatedMutation
                    || self.scope.resolution != ScopeResolution::Resolved
                    || self.scope.effective.len() != 1
                    || self.risk != RiskLevel::Low
                    || self.model_turn_budget.before_first_edit > 1
                    || self.model_turn_budget.successful_path_total > 2 =>
            {
                return Err("fast mutation requires one low-risk resolved write scope");
            }
            ExecutionLane::StandardMutation
                if self.strategy != ExecutionStrategy::CoordinatedMutation =>
            {
                return Err("standard mutation requires mutating strategy");
            }
            _ => {}
        }
        if self.strategy != ExecutionStrategy::CoordinatedMutation
            && self
                .tasks
                .iter()
                .any(|task| task.kind == TaskKind::Implementation)
        {
            return Err("read-only planning cannot contain an implementation task");
        }
        let speculative_implementation_allowed = self.lane == ExecutionLane::StandardMutation
            && self.risk == RiskLevel::Medium
            && self.confidence_milli >= 850
            && self.scope.resolution == ScopeResolution::Resolved
            && !self.scope.effective.is_empty()
            && !self.scope.effective.iter().any(|path| path == "repository");
        if self.tasks.iter().any(|planned| planned.task.speculative)
            && (!speculative_implementation_allowed
                || self.tasks.iter().any(|planned| {
                    planned.task.speculative && planned.kind != TaskKind::Implementation
                }))
        {
            return Err("speculative scheduler tasks violate the bounded eligibility policy");
        }
        let tasks = self
            .tasks
            .iter()
            .map(|task| task.task.clone())
            .collect::<Vec<_>>();
        if self.strategy == ExecutionStrategy::Direct {
            if !tasks.is_empty() {
                return Err("direct execution cannot advertise scheduler tasks");
            }
        } else {
            validate_graph(&canonical_tasks(tasks)?)?;
        }
        for planned in &self.tasks {
            if planned.context_fingerprint
                != hash(&(
                    &planned.task,
                    planned.kind,
                    &planned.title,
                    planned.cancellation_authority,
                ))
            {
                return Err("planned task context fingerprint does not match its contract");
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn dispatch_tasks(&self) -> Vec<Task> {
        self.tasks.iter().map(|task| task.task.clone()).collect()
    }

    #[must_use]
    pub fn task(&self, kind: TaskKind) -> Option<&PlannedTask> {
        self.tasks.iter().find(|task| task.kind == kind)
    }

    #[must_use]
    pub fn uses_deterministic_preflight(&self) -> bool {
        self.lane == ExecutionLane::FastMutation
    }
}

fn default_lane_rationale() -> String {
    "legacy plan defaults to full orchestration".to_owned()
}

fn select_execution_lane(
    strategy: ExecutionStrategy,
    risk: RiskLevel,
    confidence_milli: u16,
    scope: &RepositoryScope,
    broad_scope: bool,
    high_risk_language: bool,
) -> (ExecutionLane, String) {
    if strategy == ExecutionStrategy::Direct {
        return (
            ExecutionLane::Instant,
            "conversation intent requires no durable scheduler graph".to_owned(),
        );
    }
    if strategy == ExecutionStrategy::CoordinatedMutation
        && risk == RiskLevel::Low
        && confidence_milli >= 900
        && scope.resolution == ScopeResolution::Resolved
        && scope.effective.len() == 1
        && !broad_scope
        && !high_risk_language
    {
        return (
            ExecutionLane::FastMutation,
            "one exact low-risk write path permits deterministic preflight and targeted verification"
                .to_owned(),
        );
    }
    if strategy == ExecutionStrategy::CoordinatedMutation && risk == RiskLevel::Medium {
        return (
            ExecutionLane::StandardMutation,
            "related-file mutation retains bounded planning, review, and verification".to_owned(),
        );
    }
    (
        ExecutionLane::FullOrchestration,
        if strategy == ExecutionStrategy::CoordinatedReadOnly {
            "repository analysis retains coordinated evidence because no direct lane is authorized"
                .to_owned()
        } else {
            "high-risk, broad, ambiguous, or low-confidence work requires full orchestration"
                .to_owned()
        },
    )
}

const fn model_turn_budget(lane: ExecutionLane) -> ModelTurnBudget {
    match lane {
        ExecutionLane::Instant => ModelTurnBudget {
            before_first_edit: 1,
            successful_path_total: 1,
            repair_attempts: 0,
        },
        ExecutionLane::FastMutation => ModelTurnBudget {
            before_first_edit: 1,
            successful_path_total: 2,
            repair_attempts: 1,
        },
        ExecutionLane::StandardMutation => ModelTurnBudget {
            before_first_edit: 2,
            successful_path_total: 4,
            repair_attempts: 2,
        },
        ExecutionLane::FullOrchestration => ModelTurnBudget {
            before_first_edit: 3,
            successful_path_total: 8,
            repair_attempts: 3,
        },
    }
}

fn apply_speculation_flags(result: &mut PlanningResult) {
    let eligible = result.lane == ExecutionLane::StandardMutation
        && result.risk == RiskLevel::Medium
        && result.confidence_milli >= 850
        && result.scope.resolution == ScopeResolution::Resolved
        && !result.scope.effective.is_empty()
        && !result
            .scope
            .effective
            .iter()
            .any(|path| path == "repository");
    for planned in &mut result.tasks {
        planned.task.speculative = eligible && planned.kind == TaskKind::Implementation;
        planned.context_fingerprint = hash(&(
            &planned.task,
            planned.kind,
            &planned.title,
            CancellationAuthority::RuntimeController,
        ));
    }
}

fn planned_tasks(strategy: ExecutionStrategy, scope: &RepositoryScope) -> Vec<PlannedTask> {
    if strategy == ExecutionStrategy::Direct {
        return Vec::new();
    }
    let mut tasks = vec![
        planned_task(
            "analyze",
            TaskKind::Analysis,
            "Analyze objective and repository",
            Vec::new(),
            vec!["analysis".to_owned()],
            Vec::new(),
        ),
        planned_task(
            "risk-review",
            TaskKind::RiskReview,
            "Review risks and failure modes",
            Vec::new(),
            vec!["risk-review".to_owned()],
            Vec::new(),
        ),
    ];
    if strategy == ExecutionStrategy::CoordinatedMutation {
        tasks.push(planned_task(
            "implement",
            TaskKind::Implementation,
            "Implement within resolved repository scope",
            vec!["analyze".to_owned(), "risk-review".to_owned()],
            vec!["coding".to_owned()],
            scope.effective.clone(),
        ));
        tasks.push(planned_task(
            "review",
            TaskKind::Review,
            "Review prepared execution evidence",
            vec!["implement".to_owned()],
            vec!["review".to_owned()],
            Vec::new(),
        ));
        tasks.push(planned_task(
            "verify",
            TaskKind::Verification,
            "Verify repository after accepted execution",
            vec!["review".to_owned()],
            vec!["verification".to_owned()],
            Vec::new(),
        ));
    } else {
        tasks.push(planned_task(
            "review",
            TaskKind::Review,
            "Review coordinated read-only evidence",
            vec!["analyze".to_owned(), "risk-review".to_owned()],
            vec!["review".to_owned()],
            Vec::new(),
        ));
    }
    tasks
}

fn planned_task(
    id: &str,
    kind: TaskKind,
    title: &str,
    dependencies: Vec<String>,
    capabilities: Vec<String>,
    write_paths: Vec<String>,
) -> PlannedTask {
    let task = Task {
        id: id.to_owned(),
        dependencies,
        capabilities,
        write_paths,
        speculative: false,
    };
    PlannedTask {
        context_fingerprint: hash(&(&task, kind, title, CancellationAuthority::RuntimeController)),
        task,
        kind,
        title: title.to_owned(),
        cancellation_authority: CancellationAuthority::RuntimeController,
    }
}

fn planning_fingerprint(result: &PlanningResult) -> String {
    hash(&(
        result.intent,
        &result.requested_outcomes,
        &result.affected_components,
        &result.scope,
        result.risk,
        result.confidence_milli,
        &result.required_capabilities,
        result.strategy,
        result.lane,
        &result.lane_rationale,
        result.model_turn_budget,
        &result.tasks,
    ))
}

fn legacy_planning_fingerprint(result: &PlanningResult) -> String {
    hash(&(
        result.intent,
        &result.requested_outcomes,
        &result.affected_components,
        &result.scope,
        result.risk,
        result.confidence_milli,
        &result.required_capabilities,
        result.strategy,
        &result.tasks,
    ))
}

fn lexical_words(value: &str) -> BTreeSet<String> {
    value
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '-')
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

fn contains_mutation_verb(words: &BTreeSet<String>) -> bool {
    const VERBS: &[&str] = &[
        "add",
        "build",
        "change",
        "correct",
        "create",
        "delete",
        "edit",
        "fix",
        "implement",
        "make",
        "migrate",
        "modify",
        "patch",
        "refactor",
        "remove",
        "rename",
        "repair",
        "replace",
        "rewrite",
        "update",
        "upgrade",
        "write",
    ];
    VERBS.iter().any(|verb| words.contains(*verb))
}

fn contains_phrase(value: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|phrase| value.contains(phrase))
}

fn candidate_paths(value: &str) -> impl Iterator<Item = String> + '_ {
    value.split_whitespace().filter_map(|token| {
        let candidate = token
            .trim_matches(|character: char| {
                !character.is_ascii_alphanumeric()
                    && !matches!(character, '/' | '\\' | '.' | '-' | '_')
            })
            .replace('\\', "/");
        let candidate = candidate.trim_start_matches("./");
        let looks_like_path = candidate.contains('/')
            || candidate
                .rsplit('/')
                .next()
                .and_then(|name| name.rsplit_once('.'))
                .is_some_and(|(stem, extension)| !stem.is_empty() && !extension.is_empty());
        (looks_like_path && !candidate.contains("://"))
            .then(|| candidate.to_owned())
            .and_then(|candidate| normalize_path(&candidate))
    })
}

fn normalize_path(value: &str) -> Option<String> {
    let value = value
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_owned();
    (!value.is_empty()
        && !value.starts_with('/')
        && !value.split('/').any(|part| matches!(part, "" | "..")))
    .then_some(value)
}

fn path_exists(candidate: &str, repository_paths: &[String]) -> bool {
    repository_paths.iter().any(|path| {
        path == candidate
            || path
                .strip_prefix(candidate)
                .is_some_and(|remainder| remainder.starts_with('/'))
    })
}

fn affected_components(scope: &RepositoryScope, repository_paths: &[String]) -> Vec<String> {
    let sources = if scope.effective.is_empty() {
        &scope.requested
    } else {
        &scope.effective
    };
    let mut components = sources
        .iter()
        .filter_map(|path| path.split('/').next())
        .filter(|component| !component.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if components.is_empty() && !repository_paths.is_empty() {
        components.insert("repository".to_owned());
    }
    components.into_iter().collect()
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerTaskState {
    Pending { attempts: u32 },
    Running { worker_id: String, attempt: u32 },
    Succeeded { evidence: String },
    Failed { attempts: u32, reason: String },
    Cancelled { attempts: u32, reason: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LedgerTaskView {
    pub id: String,
    pub title: String,
    pub kind: TaskKind,
    pub state: LedgerTaskState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct LedgerTaskDefinition {
    order: u32,
    id: String,
    title: String,
    kind: TaskKind,
    dependencies: Vec<String>,
    context_fingerprint: String,
    cancellation_authority: CancellationAuthority,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct DurableExecutionLedger {
    schema_version: u16,
    plan_fingerprint: String,
    tasks: BTreeMap<String, LedgerTaskDefinition>,
    states: BTreeMap<String, LedgerTaskState>,
    revision: u64,
    fingerprint: String,
}

pub struct ExecutionLedger {
    path: PathBuf,
    state: DurableExecutionLedger,
}

impl ExecutionLedger {
    pub fn open_or_create(path: impl Into<PathBuf>, plan: &PlanningResult) -> Result<Self, String> {
        plan.validate().map_err(str::to_owned)?;
        let path = path.into();
        if path.is_file() {
            let state: DurableExecutionLedger =
                serde_json::from_slice(&fs::read(&path).map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())?;
            let ledger = Self { path, state };
            ledger.validate()?;
            if ledger.state.plan_fingerprint != plan.fingerprint {
                return Err(
                    "durable execution ledger belongs to a different accepted plan".to_owned(),
                );
            }
            return Ok(ledger);
        }
        let tasks = plan
            .tasks
            .iter()
            .enumerate()
            .map(|(order, planned)| {
                (
                    planned.task.id.clone(),
                    LedgerTaskDefinition {
                        order: u32::try_from(order).unwrap_or(u32::MAX),
                        id: planned.task.id.clone(),
                        title: planned.title.clone(),
                        kind: planned.kind,
                        dependencies: planned.task.dependencies.clone(),
                        context_fingerprint: planned.context_fingerprint.clone(),
                        cancellation_authority: planned.cancellation_authority,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let states = tasks
            .keys()
            .map(|id| (id.clone(), LedgerTaskState::Pending { attempts: 0 }))
            .collect::<BTreeMap<_, _>>();
        let mut ledger = Self {
            path,
            state: DurableExecutionLedger {
                schema_version: 1,
                plan_fingerprint: plan.fingerprint.clone(),
                tasks,
                states,
                revision: 0,
                fingerprint: String::new(),
            },
        };
        ledger.refresh();
        ledger.persist()?;
        Ok(ledger)
    }

    pub fn recover_interrupted(&mut self) -> Result<Vec<String>, String> {
        let mut recovered = Vec::new();
        for (task_id, state) in &mut self.state.states {
            if let LedgerTaskState::Running { attempt, .. } = state {
                let attempts = *attempt;
                *state = LedgerTaskState::Pending { attempts };
                recovered.push(task_id.clone());
            }
        }
        if !recovered.is_empty() {
            self.commit()?;
        }
        Ok(recovered)
    }

    pub fn begin(&mut self, task_id: &str, worker_id: &str) -> Result<(), String> {
        if worker_id.trim().is_empty() {
            return Err("scheduler worker identity cannot be empty".to_owned());
        }
        let definition = self
            .state
            .tasks
            .get(task_id)
            .ok_or_else(|| format!("unsupported task type or identifier: {task_id}"))?;
        if definition.dependencies.iter().any(|dependency| {
            !matches!(
                self.state.states.get(dependency),
                Some(LedgerTaskState::Succeeded { .. })
            )
        }) {
            return Err(format!(
                "task {task_id} cannot start before its durable dependencies"
            ));
        }
        let next = match self.state.states.get(task_id) {
            Some(LedgerTaskState::Pending { attempts }) => LedgerTaskState::Running {
                worker_id: worker_id.to_owned(),
                attempt: attempts.saturating_add(1),
            },
            Some(LedgerTaskState::Succeeded { .. }) => return Ok(()),
            Some(LedgerTaskState::Running { .. }) => return Ok(()),
            Some(LedgerTaskState::Failed { .. } | LedgerTaskState::Cancelled { .. }) => {
                return Err(format!("terminal task {task_id} cannot be dispatched"));
            }
            None => return Err(format!("task {task_id} has no durable state")),
        };
        self.state.states.insert(task_id.to_owned(), next);
        self.commit()
    }

    pub fn succeed(&mut self, task_id: &str, evidence: impl Into<String>) -> Result<(), String> {
        let evidence = evidence.into();
        if evidence.trim().is_empty() {
            return Err("successful task requires terminal evidence".to_owned());
        }
        match self.state.states.get(task_id) {
            Some(LedgerTaskState::Running { .. } | LedgerTaskState::Pending { .. }) => {}
            Some(LedgerTaskState::Succeeded { .. }) => return Ok(()),
            Some(LedgerTaskState::Failed { .. } | LedgerTaskState::Cancelled { .. }) => {
                return Err(format!("terminal task {task_id} cannot become successful"));
            }
            None => return Err(format!("task {task_id} has no durable state")),
        }
        self.state
            .states
            .insert(task_id.to_owned(), LedgerTaskState::Succeeded { evidence });
        self.commit()
    }

    pub fn succeed_with_evidence(
        &mut self,
        task_id: &str,
        evidence: impl Into<String>,
        dependency: &EvidenceDependency,
        bundle: &EvidenceBundle,
    ) -> Result<(), String> {
        dependency
            .validate(bundle)
            .map_err(|error| error.to_string())?;
        self.succeed(
            task_id,
            format!(
                "{}
typed_evidence_dependency={}
bundle={}",
                evidence.into(),
                dependency.fingerprint,
                bundle.fingerprint
            ),
        )
    }

    pub fn fail(&mut self, task_id: &str, reason: impl Into<String>) -> Result<(), String> {
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err("failed task requires terminal evidence".to_owned());
        }
        let attempts = match self.state.states.get(task_id) {
            Some(LedgerTaskState::Pending { attempts }) => *attempts,
            Some(LedgerTaskState::Running { attempt, .. }) => *attempt,
            Some(LedgerTaskState::Failed { .. }) => return Ok(()),
            Some(LedgerTaskState::Succeeded { .. } | LedgerTaskState::Cancelled { .. }) => {
                return Err(format!("terminal task {task_id} cannot become failed"));
            }
            None => return Err(format!("task {task_id} has no durable state")),
        };
        self.state.states.insert(
            task_id.to_owned(),
            LedgerTaskState::Failed { attempts, reason },
        );
        self.commit()
    }

    pub fn cancel_remaining(&mut self, reason: impl Into<String>) -> Result<Vec<String>, String> {
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err("cancellation requires terminal evidence".to_owned());
        }
        let mut cancelled = Vec::new();
        for (task_id, state) in &mut self.state.states {
            let attempts = match state {
                LedgerTaskState::Pending { attempts } => Some(*attempts),
                LedgerTaskState::Running { attempt, .. } => Some(*attempt),
                _ => None,
            };
            if let Some(attempts) = attempts {
                *state = LedgerTaskState::Cancelled {
                    attempts,
                    reason: reason.clone(),
                };
                cancelled.push(task_id.clone());
            }
        }
        if !cancelled.is_empty() {
            self.commit()?;
        }
        Ok(cancelled)
    }

    #[must_use]
    pub fn views(&self) -> Vec<LedgerTaskView> {
        let mut definitions = self.state.tasks.values().collect::<Vec<_>>();
        definitions.sort_by_key(|definition| definition.order);
        definitions
            .into_iter()
            .filter_map(|definition| {
                self.state
                    .states
                    .get(&definition.id)
                    .cloned()
                    .map(|state| LedgerTaskView {
                        id: definition.id.clone(),
                        title: definition.title.clone(),
                        kind: definition.kind,
                        state,
                    })
            })
            .collect()
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.state.schema_version != 1
            || self.state.tasks.len() != self.state.states.len()
            || self.state.fingerprint != ledger_fingerprint(&self.state)
        {
            return Err("durable execution ledger is incomplete or corrupted".to_owned());
        }
        if self
            .state
            .tasks
            .keys()
            .any(|task_id| !self.state.states.contains_key(task_id))
        {
            return Err("durable execution ledger has missing task state".to_owned());
        }
        Ok(())
    }

    fn commit(&mut self) -> Result<(), String> {
        self.state.revision = self.state.revision.saturating_add(1);
        self.refresh();
        self.persist()
    }

    fn refresh(&mut self) {
        self.state.fingerprint = ledger_fingerprint(&self.state);
    }

    fn persist(&self) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "execution ledger path has no parent".to_owned())?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let temporary = self.path.with_extension("json.tmp");
        fs::write(
            &temporary,
            serde_json::to_vec_pretty(&self.state).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        #[cfg(windows)]
        if self.path.exists() {
            fs::remove_file(&self.path).map_err(|error| error.to_string())?;
        }
        fs::rename(temporary, &self.path).map_err(|error| error.to_string())
    }
}

fn ledger_fingerprint(state: &DurableExecutionLedger) -> String {
    hash(&(
        state.schema_version,
        &state.plan_fingerprint,
        &state.tasks,
        &state.states,
        state.revision,
    ))
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Task {
    pub id: String,
    pub dependencies: Vec<String>,
    pub capabilities: Vec<String>,
    pub write_paths: Vec<String>,
    pub speculative: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Worker {
    pub id: String,
    pub capabilities: Vec<String>,
    pub healthy: bool,
    pub capacity: u16,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Assignment {
    pub task_id: String,
    pub worker_id: String,
    pub speculative: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Schedule {
    pub waves: Vec<Vec<Assignment>>,
    pub fingerprint: String,
}

pub fn schedule(tasks: Vec<Task>, workers: Vec<Worker>) -> Result<Schedule, &'static str> {
    let tasks = canonical_tasks(tasks)?;
    let workers = canonical_workers(workers)?;
    validate_graph(&tasks)?;

    let mut complete = BTreeSet::new();
    let mut remaining = tasks.keys().cloned().collect::<BTreeSet<_>>();
    let mut waves = Vec::new();

    while !remaining.is_empty() {
        let ready = remaining
            .iter()
            .filter(|id| {
                tasks[*id]
                    .dependencies
                    .iter()
                    .all(|dependency| complete.contains(dependency))
            })
            .cloned()
            .collect::<Vec<_>>();
        if ready.is_empty() {
            return Err("task graph cannot make progress");
        }

        let mut capacity = worker_capacity(&workers);
        let mut paths = BTreeSet::new();
        let mut wave = Vec::new();
        for id in ready {
            let task = &tasks[&id];
            if task.write_paths.iter().any(|path| paths.contains(path)) {
                continue;
            }
            let worker = workers.values().find(|worker| {
                worker.healthy
                    && capacity.get(&worker.id).copied().unwrap_or(0) > 0
                    && supports(worker, task)
            });
            if let Some(worker) = worker {
                if let Some(value) = capacity.get_mut(&worker.id) {
                    *value = value.saturating_sub(1);
                }
                paths.extend(task.write_paths.iter().cloned());
                wave.push(Assignment {
                    task_id: id,
                    worker_id: worker.id.clone(),
                    speculative: task.speculative,
                });
            }
        }
        if wave.is_empty() {
            return Err("no healthy capable worker can execute a ready task");
        }
        wave.sort_by(|a, b| {
            a.task_id
                .cmp(&b.task_id)
                .then(a.worker_id.cmp(&b.worker_id))
        });
        for assignment in &wave {
            remaining.remove(&assignment.task_id);
            complete.insert(assignment.task_id.clone());
        }
        waves.push(wave);
    }

    Ok(Schedule {
        fingerprint: hash(&waves),
        waves,
    })
}

pub fn overlapping_paths(tasks: &[Task]) -> Result<BTreeMap<String, Vec<String>>, &'static str> {
    let tasks = canonical_tasks(tasks.to_vec())?;
    let mut paths: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for task in tasks.values() {
        for path in &task.write_paths {
            paths.entry(path.clone()).or_default().push(task.id.clone());
        }
    }
    paths.retain(|_, ids| ids.len() > 1);
    Ok(paths)
}

pub fn replacement(
    task: &Task,
    unavailable: &str,
    workers: &[Worker],
) -> Result<String, &'static str> {
    validate_task(task)?;
    canonical_workers(workers.to_vec())?
        .values()
        .find(|worker| {
            worker.id != unavailable
                && worker.healthy
                && worker.capacity > 0
                && supports(worker, task)
        })
        .map(|worker| worker.id.clone())
        .ok_or("no replacement worker is available")
}

pub fn obsolete_speculation(assignments: &[Assignment], invalidated: &[String]) -> Vec<String> {
    let invalidated = invalidated.iter().collect::<BTreeSet<_>>();
    let mut result = assignments
        .iter()
        .filter(|assignment| assignment.speculative && invalidated.contains(&assignment.task_id))
        .map(|assignment| assignment.task_id.clone())
        .collect::<Vec<_>>();
    result.sort();
    result.dedup();
    result
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum TaskState {
    Pending { attempts: u32 },
    Running { worker_id: String, attempt: u32 },
    Succeeded,
    Failed { attempts: u32, reason: String },
}

/// Durable execution-time scheduler layered on top of the static planner.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DynamicSchedule {
    tasks: BTreeMap<String, Task>,
    workers: BTreeMap<String, Worker>,
    states: BTreeMap<String, TaskState>,
    max_attempts: u32,
    fingerprint: String,
}

impl DynamicSchedule {
    pub fn new(
        tasks: Vec<Task>,
        workers: Vec<Worker>,
        max_attempts: u32,
    ) -> Result<Self, &'static str> {
        if max_attempts == 0 {
            return Err("retry limit must be non-zero");
        }
        let tasks = canonical_tasks(tasks)?;
        let workers = canonical_workers(workers)?;
        validate_graph(&tasks)?;
        let states = tasks
            .keys()
            .map(|id| (id.clone(), TaskState::Pending { attempts: 0 }))
            .collect();
        let mut schedule = Self {
            tasks,
            workers,
            states,
            max_attempts,
            fingerprint: String::new(),
        };
        schedule.refresh();
        Ok(schedule)
    }

    pub fn dispatch_ready(&mut self) -> Result<Vec<Assignment>, &'static str> {
        self.validate()?;
        let succeeded = self
            .states
            .iter()
            .filter_map(|(id, state)| matches!(state, TaskState::Succeeded).then_some(id.clone()))
            .collect::<BTreeSet<_>>();
        let mut capacity = worker_capacity(&self.workers);
        for state in self.states.values() {
            if let TaskState::Running { worker_id, .. } = state {
                if let Some(value) = capacity.get_mut(worker_id) {
                    *value = value.saturating_sub(1);
                }
            }
        }

        let mut claimed_paths = self.running_paths();
        let mut assignments = Vec::new();
        for (task_id, task) in &self.tasks {
            let attempts = match self.states.get(task_id) {
                Some(TaskState::Pending { attempts }) => *attempts,
                _ => continue,
            };
            if !task
                .dependencies
                .iter()
                .all(|dependency| succeeded.contains(dependency))
                || task
                    .write_paths
                    .iter()
                    .any(|path| claimed_paths.contains(path))
            {
                continue;
            }
            let worker = self.workers.values().find(|worker| {
                worker.healthy
                    && capacity.get(&worker.id).copied().unwrap_or(0) > 0
                    && supports(worker, task)
            });
            let Some(worker) = worker else { continue };
            let worker_id = worker.id.clone();
            self.states.insert(
                task_id.clone(),
                TaskState::Running {
                    worker_id: worker_id.clone(),
                    attempt: attempts.saturating_add(1),
                },
            );
            if let Some(value) = capacity.get_mut(&worker_id) {
                *value = value.saturating_sub(1);
            }
            claimed_paths.extend(task.write_paths.iter().cloned());
            assignments.push(Assignment {
                task_id: task_id.clone(),
                worker_id,
                speculative: task.speculative,
            });
        }
        self.refresh();
        Ok(assignments)
    }

    pub fn complete(&mut self, task_id: &str, worker_id: &str) -> Result<(), &'static str> {
        self.running_attempt(task_id, worker_id)?;
        self.states.insert(task_id.to_owned(), TaskState::Succeeded);
        self.refresh();
        Ok(())
    }

    pub fn fail(
        &mut self,
        task_id: &str,
        worker_id: &str,
        reason: impl Into<String>,
        retryable: bool,
    ) -> Result<(), &'static str> {
        let attempt = self.running_attempt(task_id, worker_id)?;
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err("failure reason cannot be empty");
        }
        let state = if retryable && attempt < self.max_attempts {
            TaskState::Pending { attempts: attempt }
        } else {
            TaskState::Failed {
                attempts: attempt,
                reason,
            }
        };
        self.states.insert(task_id.to_owned(), state);
        self.refresh();
        Ok(())
    }

    pub fn reopen_succeeded(&mut self, task_id: &str) -> Result<(), &'static str> {
        match self.states.get(task_id) {
            Some(TaskState::Succeeded) => {
                self.states
                    .insert(task_id.to_owned(), TaskState::Pending { attempts: 0 });
                self.refresh();
                Ok(())
            }
            Some(_) => Err("only a succeeded task can be reopened"),
            None => Err("task does not exist"),
        }
    }

    pub fn set_worker_health(
        &mut self,
        worker_id: &str,
        healthy: bool,
    ) -> Result<(), &'static str> {
        self.workers
            .get_mut(worker_id)
            .ok_or("worker does not exist")?
            .healthy = healthy;
        if !healthy {
            let interrupted = self
                .states
                .iter()
                .filter_map(|(task_id, state)| match state {
                    TaskState::Running {
                        worker_id: assigned,
                        attempt,
                    } if assigned == worker_id => Some((task_id.clone(), *attempt)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            for (task_id, attempts) in interrupted {
                self.states.insert(task_id, TaskState::Pending { attempts });
            }
        }
        self.refresh();
        Ok(())
    }

    pub fn state(&self, task_id: &str) -> Option<&TaskState> {
        self.states.get(task_id)
    }

    #[must_use]
    pub fn tasks_with_state(&self) -> Vec<(Task, TaskState)> {
        self.tasks
            .iter()
            .filter_map(|(id, task)| {
                self.states
                    .get(id)
                    .cloned()
                    .map(|state| (task.clone(), state))
            })
            .collect()
    }

    pub fn is_complete(&self) -> bool {
        self.states
            .values()
            .all(|state| matches!(state, TaskState::Succeeded))
    }

    pub fn has_terminal_failure(&self) -> bool {
        self.states
            .values()
            .any(|state| matches!(state, TaskState::Failed { .. }))
    }

    pub fn blocked_tasks(&self) -> Vec<String> {
        let failed = self
            .states
            .iter()
            .filter_map(|(id, state)| matches!(state, TaskState::Failed { .. }).then_some(id))
            .collect::<BTreeSet<_>>();
        self.tasks
            .iter()
            .filter_map(|(id, task)| {
                (matches!(self.states.get(id), Some(TaskState::Pending { .. }))
                    && task
                        .dependencies
                        .iter()
                        .any(|dependency| failed.contains(dependency)))
                .then_some(id.clone())
            })
            .collect()
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.max_attempts == 0 || self.tasks.len() != self.states.len() {
            return Err("dynamic schedule state is incomplete");
        }
        let expected = hash(&(&self.tasks, &self.workers, &self.states, self.max_attempts));
        if expected != self.fingerprint {
            return Err("dynamic schedule fingerprint does not match its contents");
        }
        Ok(())
    }

    fn running_attempt(&self, task_id: &str, worker_id: &str) -> Result<u32, &'static str> {
        match self.states.get(task_id) {
            Some(TaskState::Running {
                worker_id: assigned,
                attempt,
            }) if assigned == worker_id => Ok(*attempt),
            Some(TaskState::Running { .. }) => Err("task is owned by a different worker"),
            Some(_) => Err("task is not running"),
            None => Err("task does not exist"),
        }
    }

    fn running_paths(&self) -> BTreeSet<String> {
        self.states
            .iter()
            .filter_map(|(task_id, state)| {
                matches!(state, TaskState::Running { .. }).then_some(&self.tasks[task_id])
            })
            .flat_map(|task| task.write_paths.iter().cloned())
            .collect()
    }

    fn refresh(&mut self) {
        self.fingerprint = hash(&(&self.tasks, &self.workers, &self.states, self.max_attempts));
    }
}

fn canonical_tasks(tasks: Vec<Task>) -> Result<BTreeMap<String, Task>, &'static str> {
    if tasks.is_empty() {
        return Err("at least one task is required");
    }
    let mut result = BTreeMap::new();
    for mut task in tasks {
        task.dependencies.sort();
        task.dependencies.dedup();
        task.capabilities.sort();
        task.capabilities.dedup();
        task.write_paths.sort();
        task.write_paths.dedup();
        validate_task(&task)?;
        if result.insert(task.id.clone(), task).is_some() {
            return Err("task identifiers must be unique");
        }
    }
    Ok(result)
}

fn canonical_workers(workers: Vec<Worker>) -> Result<BTreeMap<String, Worker>, &'static str> {
    if workers.is_empty() {
        return Err("at least one worker is required");
    }
    let mut result = BTreeMap::new();
    for mut worker in workers {
        worker.capabilities.sort();
        worker.capabilities.dedup();
        if worker.id.trim().is_empty() || worker.capacity == 0 {
            return Err("worker identifier and capacity must be valid");
        }
        if result.insert(worker.id.clone(), worker).is_some() {
            return Err("worker identifiers must be unique");
        }
    }
    Ok(result)
}

fn validate_task(task: &Task) -> Result<(), &'static str> {
    if task.id.trim().is_empty() {
        return Err("task identifier cannot be empty");
    }
    if task.dependencies.contains(&task.id) {
        return Err("task cannot depend on itself");
    }
    if task.write_paths.iter().any(|path| {
        path.is_empty() || path.starts_with('/') || path.split('/').any(|part| part == "..")
    }) {
        return Err("write paths must be workspace relative");
    }
    Ok(())
}

fn validate_graph(tasks: &BTreeMap<String, Task>) -> Result<(), &'static str> {
    for task in tasks.values() {
        if task
            .dependencies
            .iter()
            .any(|dependency| !tasks.contains_key(dependency))
        {
            return Err("task dependency does not exist");
        }
    }
    let mut done = BTreeSet::new();
    loop {
        let before = done.len();
        for task in tasks.values() {
            if task
                .dependencies
                .iter()
                .all(|dependency| done.contains(dependency))
            {
                done.insert(task.id.clone());
            }
        }
        if done.len() == tasks.len() {
            return Ok(());
        }
        if done.len() == before {
            return Err("task dependency graph contains a cycle");
        }
    }
}

fn worker_capacity(workers: &BTreeMap<String, Worker>) -> BTreeMap<String, u16> {
    workers
        .values()
        .filter(|worker| worker.healthy)
        .map(|worker| (worker.id.clone(), worker.capacity))
        .collect()
}

fn supports(worker: &Worker, task: &Task) -> bool {
    task.capabilities
        .iter()
        .all(|capability| worker.capabilities.binary_search(capability).is_ok())
}

fn hash<T: Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, dependencies: &[&str], path: &str) -> Task {
        Task {
            id: id.into(),
            dependencies: dependencies.iter().map(|value| (*value).into()).collect(),
            capabilities: vec!["rust".into()],
            write_paths: vec![path.into()],
            speculative: false,
        }
    }

    fn worker(id: &str) -> Worker {
        Worker {
            id: id.into(),
            capabilities: vec!["rust".into()],
            healthy: true,
            capacity: 1,
        }
    }

    #[test]
    fn scheduler_rejects_invalid_evidence_dependency() {
        use medusa_evidence::{EvidenceBundle, EvidenceDependency};
        let directory = tempfile::tempdir().expect("tempdir");
        let planned = plan_typed(PlannerInput {
            objective: "Fix src/lib.rs".to_owned(),
            attachment_count: 0,
            repository_paths: vec!["src/lib.rs".to_owned()],
        })
        .expect("plan");
        let mut ledger =
            ExecutionLedger::open_or_create(directory.path().join("execution.json"), &planned)
                .expect("ledger");
        ledger.begin("analyze", "planner").expect("begin");
        let bundle = EvidenceBundle::new("repo", "commit");
        let invalid = EvidenceDependency {
            bundle_fingerprint: "stale".to_owned(),
            decision_ids: Vec::new(),
            fingerprint: "corrupt".to_owned(),
        };
        assert!(
            ledger
                .succeed_with_evidence("analyze", "summary", &invalid, &bundle)
                .is_err()
        );
    }

    #[test]
    fn independent_tasks_run_in_parallel() {
        let result = schedule(
            vec![task("a", &[], "a.rs"), task("b", &[], "b.rs")],
            vec![worker("one"), worker("two")],
        )
        .unwrap();
        assert_eq!(result.waves.len(), 1);
        assert_eq!(result.waves[0].len(), 2);
    }

    #[test]
    fn dependencies_and_path_conflicts_create_new_waves() {
        let dependent = schedule(
            vec![task("a", &[], "a.rs"), task("b", &["a"], "b.rs")],
            vec![worker("one"), worker("two")],
        )
        .unwrap();
        assert_eq!(dependent.waves.len(), 2);
        let conflict = schedule(
            vec![task("a", &[], "same.rs"), task("b", &[], "same.rs")],
            vec![worker("one"), worker("two")],
        )
        .unwrap();
        assert_eq!(conflict.waves.len(), 2);
    }

    #[test]
    fn scheduling_is_deterministic_and_supports_reassignment() {
        let tasks = vec![task("a", &[], "a.rs"), task("b", &[], "b.rs")];
        let workers = vec![worker("one"), worker("two")];
        assert_eq!(
            schedule(tasks.clone(), workers.clone()).unwrap(),
            schedule(
                tasks.into_iter().rev().collect(),
                workers.into_iter().rev().collect()
            )
            .unwrap()
        );
        assert_eq!(
            replacement(
                &task("a", &[], "a.rs"),
                "one",
                &[worker("one"), worker("two")]
            )
            .unwrap(),
            "two"
        );
    }

    #[test]
    fn dynamic_completion_releases_dependencies() {
        let mut runtime = DynamicSchedule::new(
            vec![
                task("plan", &[], "plan.md"),
                task("code", &["plan"], "src/lib.rs"),
            ],
            vec![worker("one")],
            2,
        )
        .unwrap();
        assert_eq!(runtime.dispatch_ready().unwrap()[0].task_id, "plan");
        runtime.complete("plan", "one").unwrap();
        assert_eq!(runtime.dispatch_ready().unwrap()[0].task_id, "code");
    }

    #[test]
    fn dynamic_worker_failure_requeues_with_attempt_history() {
        let mut runtime = DynamicSchedule::new(
            vec![task("code", &[], "src/lib.rs")],
            vec![worker("one"), worker("two")],
            2,
        )
        .unwrap();
        assert_eq!(runtime.dispatch_ready().unwrap()[0].worker_id, "one");
        runtime.set_worker_health("one", false).unwrap();
        assert_eq!(runtime.dispatch_ready().unwrap()[0].worker_id, "two");
        assert_eq!(
            runtime.state("code"),
            Some(&TaskState::Running {
                worker_id: "two".into(),
                attempt: 2,
            })
        );
    }

    #[test]
    fn dynamic_retry_limit_blocks_dependents() {
        let mut runtime = DynamicSchedule::new(
            vec![
                task("code", &[], "src/lib.rs"),
                task("test", &["code"], "tests/a.rs"),
            ],
            vec![worker("one")],
            2,
        )
        .unwrap();
        runtime.dispatch_ready().unwrap();
        runtime.fail("code", "one", "first", true).unwrap();
        runtime.dispatch_ready().unwrap();
        runtime.fail("code", "one", "second", true).unwrap();
        assert!(runtime.has_terminal_failure());
        assert_eq!(runtime.blocked_tasks(), vec!["test"]);
    }

    #[test]
    fn mutation_vocabulary_recognizes_fix() {
        let words = lexical_words("fix the failing tests");
        assert!(contains_mutation_verb(&words));
    }

    #[test]
    fn typed_planner_fails_closed_when_mutation_scope_is_unknown() {
        let planned = plan_typed(PlannerInput {
            objective: "Fix the failing tests".to_owned(),
            attachment_count: 0,
            repository_paths: vec!["src/lib.rs".to_owned()],
        })
        .unwrap();
        assert_eq!(planned.intent, PlanningIntent::MutationRequested);
        assert_eq!(planned.scope.resolution, ScopeResolution::Unresolved);
        assert_eq!(planned.strategy, ExecutionStrategy::CoordinatedReadOnly);
        assert!(planned.task(TaskKind::Implementation).is_none());
    }

    #[test]
    fn typed_planner_resolves_synonyms_and_multi_component_scope() {
        let planned = plan_typed(PlannerInput {
            objective: "Correct src/lib.rs and repair crates/worker/src/lib.rs".to_owned(),
            attachment_count: 0,
            repository_paths: vec![
                "src/lib.rs".to_owned(),
                "crates/worker/src/lib.rs".to_owned(),
            ],
        })
        .unwrap();
        assert_eq!(planned.strategy, ExecutionStrategy::CoordinatedMutation);
        assert_eq!(
            planned.scope.effective,
            vec![
                "crates/worker/src/lib.rs".to_owned(),
                "src/lib.rs".to_owned()
            ]
        );
        assert!(planned.affected_components.contains(&"crates".to_owned()));
        assert!(planned.affected_components.contains(&"src".to_owned()));
    }

    #[test]
    fn typed_planner_avoids_false_positive_for_read_only_language() {
        let planned = plan_typed(PlannerInput {
            objective: "Explain how to fix src/lib.rs without changing files".to_owned(),
            attachment_count: 0,
            repository_paths: vec!["src/lib.rs".to_owned()],
        })
        .unwrap();
        assert_eq!(planned.intent, PlanningIntent::ReadOnly);
        assert_eq!(planned.strategy, ExecutionStrategy::CoordinatedReadOnly);
        assert!(planned.task(TaskKind::Implementation).is_none());
    }

    #[test]
    fn fresh_graph_public_api_risk_escalates_fast_plan_without_changing_scope() {
        let planned = plan_typed(PlannerInput {
            objective: "Fix src/lib.rs".to_owned(),
            attachment_count: 0,
            repository_paths: vec!["src/lib.rs".to_owned()],
        })
        .unwrap();
        assert_eq!(planned.lane, ExecutionLane::FastMutation);
        let enriched = apply_repository_graph_evidence(
            planned,
            vec!["src".to_owned(), "api".to_owned()],
            true,
            true,
        )
        .unwrap();
        assert_eq!(enriched.scope.effective, vec!["src/lib.rs"]);
        assert_eq!(enriched.risk, RiskLevel::High);
        assert_eq!(enriched.lane, ExecutionLane::FullOrchestration);
        assert_eq!(enriched.affected_components, vec!["api", "src"]);
    }

    #[test]
    fn single_file_localized_fix_selects_fast_mutation_budget() {
        let planned = plan_typed(PlannerInput {
            objective: "Fix src/lib.rs".to_owned(),
            attachment_count: 0,
            repository_paths: vec!["src/lib.rs".to_owned()],
        })
        .unwrap();
        assert_eq!(planned.risk, RiskLevel::Low);
        assert_eq!(planned.lane, ExecutionLane::FastMutation);
        assert_eq!(planned.model_turn_budget.before_first_edit, 1);
        assert_eq!(planned.model_turn_budget.successful_path_total, 2);
        assert!(planned.uses_deterministic_preflight());
    }

    #[test]
    fn related_file_mutation_selects_standard_lane() {
        let planned = plan_typed(PlannerInput {
            objective: "Fix src/lib.rs and src/tests.rs".to_owned(),
            attachment_count: 0,
            repository_paths: vec!["src/lib.rs".to_owned(), "src/tests.rs".to_owned()],
        })
        .unwrap();
        assert_eq!(planned.risk, RiskLevel::Medium);
        assert_eq!(planned.lane, ExecutionLane::StandardMutation);
        assert!(!planned.uses_deterministic_preflight());
    }

    #[test]
    fn security_and_repository_wide_work_select_full_orchestration() {
        for objective in [
            "Fix security policy in src/lib.rs",
            "Implement a repository-wide refactor",
            "Upgrade dependency in Cargo.toml",
        ] {
            let planned = plan_typed(PlannerInput {
                objective: objective.to_owned(),
                attachment_count: 0,
                repository_paths: vec!["src/lib.rs".to_owned(), "Cargo.toml".to_owned()],
            })
            .unwrap();
            assert_eq!(planned.lane, ExecutionLane::FullOrchestration);
            assert!(!planned.uses_deterministic_preflight());
        }
    }

    #[test]
    fn execution_ledger_recovers_running_tasks_without_phantoms() {
        let directory = tempfile::tempdir().unwrap();
        let planned = plan_typed(PlannerInput {
            objective: "Implement a repository-wide refactor".to_owned(),
            attachment_count: 0,
            repository_paths: vec!["src/lib.rs".to_owned()],
        })
        .unwrap();
        let path = directory.path().join("execution.json");
        let mut ledger = ExecutionLedger::open_or_create(&path, &planned).unwrap();
        ledger.begin("analyze", "planner").unwrap();
        drop(ledger);
        let mut restored = ExecutionLedger::open_or_create(&path, &planned).unwrap();
        assert_eq!(restored.recover_interrupted().unwrap(), vec!["analyze"]);
        assert!(matches!(
            restored.views()[0].state,
            LedgerTaskState::Pending { attempts: 1 }
        ));
    }
}
