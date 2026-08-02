from __future__ import annotations

from pathlib import Path


def replace_once(path: Path, before: str, after: str) -> None:
    text = path.read_text(encoding="utf-8")
    if text.count(before) != 1:
        raise RuntimeError(f"{path}: expected exactly one match for replacement")
    path.write_text(text.replace(before, after, 1), encoding="utf-8")


scheduler = Path("crates/medusa-multi-agent-scheduler/src/lib.rs")
replace_once(
    scheduler,
    "use std::collections::{BTreeMap, BTreeSet};\n",
    "use std::{\n    collections::{BTreeMap, BTreeSet},\n    fs,\n    path::{Path, PathBuf},\n};\n",
)

planner_block = r'''
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
    let mutation_requested = !explicitly_read_only
        && words.iter().any(|word| {
            matches!(
                word.as_str(),
                "add"
                    | "build"
                    | "change"
                    | "correct"
                    | "create"
                    | "delete"
                    | "edit"
                    | "fix"
                    | "implement"
                    | "make"
                    | "migrate"
                    | "modify"
                    | "patch"
                    | "refactor"
                    | "remove"
                    | "rename"
                    | "repair"
                    | "replace"
                    | "rewrite"
                    | "update"
                    | "upgrade"
                    | "write"
            )
        });
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
    let risk = if strategy == ExecutionStrategy::CoordinatedMutation
        && (broad_scope
            || scope.effective.len() > 2
            || words.iter().any(|word| {
                matches!(word.as_str(), "architecture" | "migration" | "release" | "security")
            }))
    {
        RiskLevel::High
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
    let tasks = planned_tasks(&input.objective, strategy, &scope);
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
        tasks,
        fingerprint: String::new(),
    };
    result.fingerprint = planning_fingerprint(&result);
    result.validate()?;
    Ok(result)
}

impl PlanningResult {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.confidence_milli > 1_000
            || self.requested_outcomes.is_empty()
            || self.requested_outcomes.iter().any(|value| value.trim().is_empty())
            || self.fingerprint != planning_fingerprint(self)
        {
            return Err("typed planning result is incomplete or corrupted");
        }
        if self.strategy == ExecutionStrategy::CoordinatedMutation
            && (self.scope.resolution != ScopeResolution::Resolved
                || self.scope.effective.is_empty())
        {
            return Err("mutating execution requires resolved effective scope");
        }
        if self.strategy != ExecutionStrategy::CoordinatedMutation
            && self
                .tasks
                .iter()
                .any(|task| task.kind == TaskKind::Implementation)
        {
            return Err("read-only planning cannot contain an implementation task");
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
}

fn planned_tasks(objective: &str, strategy: ExecutionStrategy, scope: &RepositoryScope) -> Vec<PlannedTask> {
    if strategy == ExecutionStrategy::Direct {
        return Vec::new();
    }
    let mut tasks = vec![
        planned_task("analyze", TaskKind::Analysis, "Analyze objective and repository", objective, Vec::new(), vec!["analysis".to_owned()], Vec::new()),
        planned_task("risk-review", TaskKind::RiskReview, "Review risks and failure modes", objective, Vec::new(), vec!["risk-review".to_owned()], Vec::new()),
    ];
    if strategy == ExecutionStrategy::CoordinatedMutation {
        tasks.push(planned_task(
            "implement",
            TaskKind::Implementation,
            "Implement within resolved repository scope",
            objective,
            vec!["analyze".to_owned(), "risk-review".to_owned()],
            vec!["coding".to_owned()],
            scope.effective.clone(),
        ));
        tasks.push(planned_task(
            "review",
            TaskKind::Review,
            "Review prepared execution evidence",
            objective,
            vec!["implement".to_owned()],
            vec!["review".to_owned()],
            Vec::new(),
        ));
        tasks.push(planned_task(
            "verify",
            TaskKind::Verification,
            "Verify repository after accepted execution",
            objective,
            vec!["review".to_owned()],
            vec!["verification".to_owned()],
            Vec::new(),
        ));
    } else {
        tasks.push(planned_task(
            "review",
            TaskKind::Review,
            "Review coordinated read-only evidence",
            objective,
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
    objective: &str,
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
        context_fingerprint: hash(&(
            &task,
            kind,
            title,
            CancellationAuthority::RuntimeController,
        )),
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
                return Err("durable execution ledger belongs to a different accepted plan".to_owned());
            }
            return Ok(ledger);
        }
        let tasks = plan
            .tasks
            .iter()
            .map(|planned| {
                (
                    planned.task.id.clone(),
                    LedgerTaskDefinition {
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
            return Err(format!("task {task_id} cannot start before its durable dependencies"));
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
        self.state
            .tasks
            .values()
            .filter_map(|definition| {
                self.state.states.get(&definition.id).cloned().map(|state| {
                    LedgerTaskView {
                        id: definition.id.clone(),
                        title: definition.title.clone(),
                        kind: definition.kind,
                        state,
                    }
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

'''
replace_once(
    scheduler,
    "#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]\npub struct Task {",
    planner_block + "#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]\npub struct Task {",
)

scheduler_tests = r'''

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
            vec!["crates/worker/src/lib.rs".to_owned(), "src/lib.rs".to_owned()]
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
'''
replace_once(scheduler, "\n}\n", scheduler_tests + "\n}\n")

orchestrator = r'''use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use medusa_agent::{AgentPlanStep, AgentPlanStepStatus};
use medusa_multi_agent_scheduler::{
    CancellationAuthority, ExecutionLedger, ExecutionStrategy, LedgerTaskState, PlannedTask,
    PlannerInput, PlanningResult, Schedule, Task, TaskKind, Worker, plan_typed, schedule,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{RuntimeActivity, RuntimeActivityKind, RuntimeEvent, prompt::PromptDraft};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ExecutionMode {
    Direct,
    Orchestrated,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AgentRole {
    Planner,
    Researcher,
    Implementer,
    Reviewer,
    Verifier,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DelegationPolicy {
    pub allowed: bool,
    pub max_depth: u8,
    pub max_parallel_subagents: u16,
    pub parent_must_review: bool,
    pub parent_must_integrate: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentContract {
    pub task_id: String,
    pub role: AgentRole,
    pub objective: String,
    pub dependencies: Vec<String>,
    pub allowed_write_paths: Vec<String>,
    pub required_evidence: Vec<String>,
    pub delegation: DelegationPolicy,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextPacket {
    pub task_id: String,
    pub objective: String,
    pub repository_scope: Vec<String>,
    pub dependency_outputs: BTreeMap<String, String>,
    pub authoritative_policies: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub contract: AgentContract,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProductionExecutionPlan {
    pub mode: ExecutionMode,
    pub planning: PlanningResult,
    pub tasks: Vec<Task>,
    pub schedule: Option<Schedule>,
    pub contracts: Vec<AgentContract>,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersistedOutcome {
    pub objective_fingerprint: String,
    pub plan_fingerprint: String,
    pub verified: bool,
    pub failed: bool,
}

pub fn plan(draft: &PromptDraft) -> Result<ProductionExecutionPlan, &'static str> {
    plan_for_repository(Path::new("."), draft)
}

pub fn plan_for_repository(
    repo: &Path,
    draft: &PromptDraft,
) -> Result<ProductionExecutionPlan, &'static str> {
    let planning = plan_typed(PlannerInput {
        objective: draft.text.clone(),
        attachment_count: draft.attachments.len(),
        repository_paths: repository_paths(repo),
    })?;
    let mode = if planning.strategy == ExecutionStrategy::Direct {
        ExecutionMode::Direct
    } else {
        ExecutionMode::Orchestrated
    };
    let tasks = planning.dispatch_tasks();
    let schedule = if tasks.is_empty() {
        None
    } else {
        Some(schedule(tasks.clone(), workers_for(&tasks))?)
    };
    let contracts = planning
        .tasks
        .iter()
        .map(|task| contract_for(&draft.text, task))
        .collect();
    let fingerprint = planning.fingerprint.clone();
    Ok(ProductionExecutionPlan {
        mode,
        planning,
        tasks,
        schedule,
        contracts,
        fingerprint,
    })
}

#[must_use]
pub fn requires_mutation(plan: &ProductionExecutionPlan) -> bool {
    plan.planning.strategy == ExecutionStrategy::CoordinatedMutation
        && plan.planning.task(TaskKind::Implementation).is_some()
}

pub fn runtime_context(plan: &ProductionExecutionPlan) -> String {
    if plan.mode == ExecutionMode::Direct {
        return "Production execution mode: direct. No scheduler tasks or mutation authority were created."
            .to_owned();
    }
    let contracts = plan
        .planning
        .tasks
        .iter()
        .map(|planned| {
            format!(
                "- {} ({:?}): dependencies={:?}; writes={:?}; context={}",
                planned.task.id,
                planned.kind,
                planned.task.dependencies,
                planned.task.write_paths,
                planned.context_fingerprint,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Production execution mode: {:?}. Intent={:?}; scope={:?}; risk={:?}; confidence={}/1000. The persisted execution ledger is the sole task authority. Unknown write scope grants no mutation authority. Every displayed task has durable dispatch and terminal evidence.\n{}",
        plan.planning.strategy,
        plan.planning.intent,
        plan.planning.scope,
        plan.planning.risk,
        plan.planning.confidence_milli,
        contracts,
    )
}

pub fn context_for_task(
    plan: &ProductionExecutionPlan,
    task_id: &str,
    dependency_outputs: BTreeMap<String, String>,
    policies: Vec<String>,
    acceptance_criteria: Vec<String>,
) -> Result<ContextPacket, &'static str> {
    let contract = plan
        .contracts
        .iter()
        .find(|contract| contract.task_id == task_id)
        .cloned()
        .ok_or("task contract does not exist")?;
    let planned = plan
        .planning
        .tasks
        .iter()
        .find(|planned| planned.task.id == task_id)
        .ok_or("typed task metadata does not exist")?;
    if dependency_outputs
        .keys()
        .any(|dependency| !contract.dependencies.contains(dependency))
    {
        return Err("context contains output from an unrelated dependency");
    }
    if contract
        .dependencies
        .iter()
        .any(|dependency| !dependency_outputs.contains_key(dependency))
    {
        return Err("context is missing a required dependency output");
    }
    let objective = contract.objective.clone();
    let repository_scope = contract.allowed_write_paths.clone();
    let fingerprint = digest(&(
        &plan.fingerprint,
        task_id,
        &objective,
        &repository_scope,
        &dependency_outputs,
        &policies,
        &acceptance_criteria,
        &contract,
        &planned.context_fingerprint,
        CancellationAuthority::RuntimeController,
    ));
    Ok(ContextPacket {
        task_id: task_id.to_owned(),
        objective,
        repository_scope,
        dependency_outputs,
        authoritative_policies: policies,
        acceptance_criteria,
        contract,
        fingerprint,
    })
}

pub fn validate_subagent_result(
    parent: &ContextPacket,
    delegated_task_id: &str,
    claimed_parent_fingerprint: &str,
    evidence: &[String],
) -> Result<(), &'static str> {
    if !parent.contract.delegation.allowed {
        return Err("subagent delegation is not allowed for this task");
    }
    if delegated_task_id != parent.task_id {
        return Err("subagent result does not match its durable task identity");
    }
    if claimed_parent_fingerprint != parent.fingerprint {
        return Err("subagent result was produced from stale or unrelated context");
    }
    if evidence.is_empty() || evidence.iter().any(|item| item.trim().is_empty()) {
        return Err("subagent result requires non-empty evidence for parent review");
    }
    Ok(())
}

pub fn events(plan: &ProductionExecutionPlan) -> Vec<RuntimeEvent> {
    if plan.mode == ExecutionMode::Direct {
        return vec![RuntimeEvent::Activity(RuntimeActivity {
            id: Some(plan.fingerprint.clone()),
            kind: RuntimeActivityKind::Progress,
            title: "Direct execution selected".to_owned(),
            details: vec!["No durable scheduler tasks were created.".to_owned()],
        })];
    }
    vec![RuntimeEvent::Activity(RuntimeActivity {
        id: Some(plan.fingerprint.clone()),
        kind: RuntimeActivityKind::Progress,
        title: "Authoritative execution graph accepted".to_owned(),
        details: vec![
            format!("{} typed tasks", plan.tasks.len()),
            format!("strategy={:?}", plan.planning.strategy),
            format!("scope={:?}", plan.planning.scope.resolution),
            format!("risk={:?}", plan.planning.risk),
            "All frontend task state is projected from the durable execution ledger."
                .to_owned(),
        ],
    })]
}

pub fn open_ledger(repo: &Path, plan: &ProductionExecutionPlan) -> Result<ExecutionLedger, String> {
    let path = repo
        .join(".medusa")
        .join("executions")
        .join(&plan.fingerprint)
        .join("execution-ledger.json");
    let mut ledger = ExecutionLedger::open_or_create(path, &plan.planning)?;
    ledger.recover_interrupted()?;
    Ok(ledger)
}

pub fn begin_kinds(
    ledger: &mut ExecutionLedger,
    plan: &ProductionExecutionPlan,
    kinds: &[TaskKind],
    worker_prefix: &str,
) -> Result<(), String> {
    for planned in plan
        .planning
        .tasks
        .iter()
        .filter(|planned| kinds.contains(&planned.kind))
    {
        ledger.begin(
            &planned.task.id,
            &format!("{worker_prefix}-{}", planned.task.id),
        )?;
    }
    Ok(())
}

pub fn succeed_kinds(
    ledger: &mut ExecutionLedger,
    plan: &ProductionExecutionPlan,
    kinds: &[TaskKind],
    evidence: &str,
) -> Result<(), String> {
    for planned in plan
        .planning
        .tasks
        .iter()
        .filter(|planned| kinds.contains(&planned.kind))
    {
        ledger.succeed(&planned.task.id, evidence)?;
    }
    Ok(())
}

pub fn fail_kinds(
    ledger: &mut ExecutionLedger,
    plan: &ProductionExecutionPlan,
    kinds: &[TaskKind],
    reason: &str,
) -> Result<(), String> {
    for planned in plan
        .planning
        .tasks
        .iter()
        .filter(|planned| kinds.contains(&planned.kind))
    {
        ledger.fail(&planned.task.id, reason)?;
    }
    Ok(())
}

pub fn projection(ledger: &ExecutionLedger) -> Vec<AgentPlanStep> {
    ledger
        .views()
        .into_iter()
        .map(|view| AgentPlanStep {
            title: view.title,
            status: match view.state {
                LedgerTaskState::Pending { .. } => AgentPlanStepStatus::Pending,
                LedgerTaskState::Running { .. } => AgentPlanStepStatus::InProgress,
                LedgerTaskState::Succeeded { .. } => AgentPlanStepStatus::Completed,
                LedgerTaskState::Failed { .. } | LedgerTaskState::Cancelled { .. } => {
                    AgentPlanStepStatus::Failed
                }
            },
        })
        .collect()
}

pub fn persist_outcome(
    repo: &Path,
    draft: &PromptDraft,
    plan: &ProductionExecutionPlan,
    verified: bool,
    failed: bool,
) -> std::io::Result<PathBuf> {
    let directory = repo.join(".medusa").join("learning");
    fs::create_dir_all(&directory)?;
    let path = directory.join("runtime-outcomes.jsonl");
    let outcome = PersistedOutcome {
        objective_fingerprint: digest(&draft.text),
        plan_fingerprint: plan.fingerprint.clone(),
        verified,
        failed,
    };
    let mut line = serde_json::to_vec(&outcome).map_err(std::io::Error::other)?;
    line.push(b'\n');
    use std::io::Write;
    let mut file = fs::OpenOptions::new().create(true).append(true).open(&path)?;
    file.write_all(&line)?;
    Ok(path)
}

fn contract_for(objective: &str, planned: &PlannedTask) -> AgentContract {
    let (role, required_evidence) = match planned.kind {
        TaskKind::Analysis => (
            AgentRole::Planner,
            vec!["repository evidence".to_owned(), "dependency-aware plan".to_owned()],
        ),
        TaskKind::RiskReview => (
            AgentRole::Researcher,
            vec!["risk inventory".to_owned(), "failure-mode evidence".to_owned()],
        ),
        TaskKind::Implementation => (
            AgentRole::Implementer,
            vec!["patch or commit evidence".to_owned(), "focused tests".to_owned()],
        ),
        TaskKind::Review => (
            AgentRole::Reviewer,
            vec!["accepted execution evidence".to_owned(), "policy compliance".to_owned()],
        ),
        TaskKind::Verification => (
            AgentRole::Verifier,
            vec!["acceptance criteria".to_owned(), "repository verification".to_owned()],
        ),
    };
    let delegation_allowed = matches!(role, AgentRole::Planner | AgentRole::Researcher);
    AgentContract {
        task_id: planned.task.id.clone(),
        role,
        objective: objective.to_owned(),
        dependencies: planned.task.dependencies.clone(),
        allowed_write_paths: planned.task.write_paths.clone(),
        required_evidence,
        delegation: DelegationPolicy {
            allowed: delegation_allowed,
            max_depth: u8::from(delegation_allowed),
            max_parallel_subagents: if delegation_allowed { 2 } else { 0 },
            parent_must_review: true,
            parent_must_integrate: true,
        },
    }
}

fn workers_for(tasks: &[Task]) -> Vec<Worker> {
    tasks
        .iter()
        .map(|task| Worker {
            id: format!("worker-{}", task.id),
            capabilities: task.capabilities.clone(),
            healthy: true,
            capacity: 1,
        })
        .collect()
}

fn repository_paths(repo: &Path) -> Vec<String> {
    fn visit(root: &Path, current: &Path, paths: &mut BTreeSet<String>) {
        if paths.len() >= 4_096 {
            return;
        }
        let Ok(entries) = fs::read_dir(current) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let relative = relative.to_string_lossy().replace('\\', "/");
            if relative == ".git"
                || relative.starts_with(".git/")
                || relative == ".medusa"
                || relative.starts_with(".medusa/")
            {
                continue;
            }
            paths.insert(relative);
            if path.is_dir() {
                visit(root, &path, paths);
            }
        }
    }
    let mut paths = BTreeSet::new();
    visit(repo, repo, &mut paths);
    paths.into_iter().collect()
}

fn digest<T: Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_conversation_avoids_orchestration() {
        let draft = PromptDraft {
            text: "Hello, explain this concept".to_owned(),
            ..PromptDraft::default()
        };
        assert_eq!(plan(&draft).unwrap().mode, ExecutionMode::Direct);
    }

    #[test]
    fn repository_wide_mutation_is_typed_and_scheduled() {
        let draft = PromptDraft {
            text: "Implement a repository-wide refactor and run all tests".to_owned(),
            ..PromptDraft::default()
        };
        let planned = plan(&draft).unwrap();
        assert_eq!(planned.mode, ExecutionMode::Orchestrated);
        assert!(requires_mutation(&planned));
        assert_eq!(planned.tasks.len(), 5);
        assert_eq!(planned.schedule.as_ref().unwrap().waves.len(), 4);
    }

    #[test]
    fn unknown_mutation_scope_stays_read_only() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("value.txt"), "41").unwrap();
        let draft = PromptDraft {
            text: "Fix the defect".to_owned(),
            ..PromptDraft::default()
        };
        let planned = plan_for_repository(directory.path(), &draft).unwrap();
        assert!(!requires_mutation(&planned));
        assert_eq!(
            planned.planning.scope.resolution,
            medusa_multi_agent_scheduler::ScopeResolution::Unresolved
        );
    }

    #[test]
    fn explicit_paths_resolve_against_repository_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("src")).unwrap();
        fs::write(directory.path().join("src/lib.rs"), "").unwrap();
        let draft = PromptDraft {
            text: "Repair src/lib.rs".to_owned(),
            ..PromptDraft::default()
        };
        let planned = plan_for_repository(directory.path(), &draft).unwrap();
        assert!(requires_mutation(&planned));
        assert_eq!(
            planned.planning.scope.effective,
            vec!["src/lib.rs".to_owned()]
        );
    }

    #[test]
    fn task_context_is_bound_to_accepted_plan() {
        let draft = PromptDraft {
            text: "Implement a repository-wide refactor".to_owned(),
            ..PromptDraft::default()
        };
        let planned = plan(&draft).unwrap();
        let packet = context_for_task(
            &planned,
            "implement",
            BTreeMap::from([
                ("analyze".to_owned(), "analysis".to_owned()),
                ("risk-review".to_owned(), "risk".to_owned()),
            ]),
            vec!["scope policy".to_owned()],
            vec!["tests pass".to_owned()],
        )
        .unwrap();
        assert!(!packet.fingerprint.is_empty());
        assert!(!packet.contract.delegation.allowed);
    }

    #[test]
    fn durable_projection_comes_from_ledger() {
        let directory = tempfile::tempdir().unwrap();
        let draft = PromptDraft {
            text: "Analyze repository architecture without changing files".to_owned(),
            ..PromptDraft::default()
        };
        let planned = plan_for_repository(directory.path(), &draft).unwrap();
        let mut ledger = open_ledger(directory.path(), &planned).unwrap();
        begin_kinds(
            &mut ledger,
            &planned,
            &[TaskKind::Analysis, TaskKind::RiskReview],
            "test",
        )
        .unwrap();
        assert!(projection(&ledger)
            .iter()
            .take(2)
            .all(|step| step.status == AgentPlanStepStatus::InProgress));
    }
}
'''
Path("crates/medusa-runtime/src/production_orchestrator.rs").write_text(orchestrator, encoding="utf-8")

runtime = Path("crates/medusa-runtime/src/lib.rs")
replace_once(
    runtime,
    "crate::production_orchestrator::plan(&draft).map_err(RuntimeError::agent)?;",
    "crate::production_orchestrator::plan_for_repository(&state.repo, &draft)\n            .map_err(RuntimeError::agent)?;",
)
replace_once(
    runtime,
    "    if execution_plan.mode == crate::production_orchestrator::ExecutionMode::Direct {\n        let _ = events.send(RuntimeEvent::Team(state.team_control.clear()));\n    } else {\n        state.team_control.clear();\n    }\n    for event in crate::production_orchestrator::events(&execution_plan) {\n        let _ = events.send(event);\n    }\n",
    "    let mut execution_ledger = if coordinated {\n        let ledger = crate::production_orchestrator::open_ledger(&state.repo, &execution_plan)\n            .map_err(RuntimeError::agent)?;\n        let projected = crate::production_orchestrator::projection(&ledger);\n        session.plan = projected.clone();\n        let _ = events.send(RuntimeEvent::Plan(projected));\n        Some(ledger)\n    } else {\n        None\n    };\n    if execution_plan.mode == crate::production_orchestrator::ExecutionMode::Direct {\n        let _ = events.send(RuntimeEvent::Team(state.team_control.clear()));\n    } else {\n        state.team_control.clear();\n    }\n    for event in crate::production_orchestrator::events(&execution_plan) {\n        let _ = events.send(event);\n    }\n",
)
replace_once(
    runtime,
    "    let coordinator_evidence = if !resuming_pending_question && coordinated {\n        Some(\n            crate::multi_agent_coordinator::run_preflight(\n                &state.repo,\n                &config,\n                state.session_api_key.clone(),\n                &execution_plan,\n                cancel,\n                &state.team_control,\n                events,\n            )\n            .map_err(RuntimeError::agent)?,\n        )\n    } else {\n        None\n    };\n",
    "    let coordinator_evidence = if !resuming_pending_question && coordinated {\n        if let Some(ledger) = execution_ledger.as_mut() {\n            crate::production_orchestrator::begin_kinds(\n                ledger,\n                &execution_plan,\n                &[\n                    medusa_multi_agent_scheduler::TaskKind::Analysis,\n                    medusa_multi_agent_scheduler::TaskKind::RiskReview,\n                ],\n                \"preflight\",\n            )\n            .map_err(RuntimeError::agent)?;\n            let _ = events.send(RuntimeEvent::Plan(\n                crate::production_orchestrator::projection(ledger),\n            ));\n        }\n        match crate::multi_agent_coordinator::run_preflight(\n            &state.repo,\n            &config,\n            state.session_api_key.clone(),\n            &execution_plan,\n            cancel,\n            &state.team_control,\n            events,\n        ) {\n            Ok(evidence) => {\n                if let Some(ledger) = execution_ledger.as_mut() {\n                    crate::production_orchestrator::succeed_kinds(\n                        ledger,\n                        &execution_plan,\n                        &[\n                            medusa_multi_agent_scheduler::TaskKind::Analysis,\n                            medusa_multi_agent_scheduler::TaskKind::RiskReview,\n                        ],\n                        \"durable preflight worker evidence recorded\",\n                    )\n                    .map_err(RuntimeError::agent)?;\n                    let _ = events.send(RuntimeEvent::Plan(\n                        crate::production_orchestrator::projection(ledger),\n                    ));\n                }\n                Some(evidence)\n            }\n            Err(error) => {\n                if let Some(ledger) = execution_ledger.as_mut() {\n                    let _ = crate::production_orchestrator::fail_kinds(\n                        ledger,\n                        &execution_plan,\n                        &[\n                            medusa_multi_agent_scheduler::TaskKind::Analysis,\n                            medusa_multi_agent_scheduler::TaskKind::RiskReview,\n                        ],\n                        &error,\n                    );\n                    let _ = events.send(RuntimeEvent::Plan(\n                        crate::production_orchestrator::projection(ledger),\n                    ));\n                }\n                return Err(RuntimeError::agent(error));\n            }\n        }\n    } else {\n        None\n    };\n",
)
replace_once(
    runtime,
    "    let implementation_evidence =\n        if crate::production_orchestrator::requires_mutation(&execution_plan) {\n            let preflight = coordinator_evidence.as_ref().ok_or_else(|| {\n                RuntimeError::agent(\"mutating execution requires coordinator preflight evidence\")\n            })?;\n            Some(\n                crate::mutating_worker_coordinator::run_implementation(\n                    &state.repo,\n                    &config,\n                    state.session_api_key.clone(),\n                    &execution_plan,\n                    preflight,\n                    cancel,\n                    (&state.team_control, events),\n                )\n                .map_err(RuntimeError::agent)?,\n            )\n        } else {\n            None\n        };\n",
    "    let implementation_evidence =\n        if crate::production_orchestrator::requires_mutation(&execution_plan) {\n            let preflight = coordinator_evidence.as_ref().ok_or_else(|| {\n                RuntimeError::agent(\"mutating execution requires coordinator preflight evidence\")\n            })?;\n            if let Some(ledger) = execution_ledger.as_mut() {\n                crate::production_orchestrator::begin_kinds(\n                    ledger,\n                    &execution_plan,\n                    &[medusa_multi_agent_scheduler::TaskKind::Implementation],\n                    \"implementation\",\n                )\n                .map_err(RuntimeError::agent)?;\n                let _ = events.send(RuntimeEvent::Plan(\n                    crate::production_orchestrator::projection(ledger),\n                ));\n            }\n            match crate::mutating_worker_coordinator::run_implementation(\n                &state.repo,\n                &config,\n                state.session_api_key.clone(),\n                &execution_plan,\n                preflight,\n                cancel,\n                (&state.team_control, events),\n            ) {\n                Ok(evidence) => {\n                    if let Some(ledger) = execution_ledger.as_mut() {\n                        crate::production_orchestrator::succeed_kinds(\n                            ledger,\n                            &execution_plan,\n                            &[medusa_multi_agent_scheduler::TaskKind::Implementation],\n                            \"isolated implementation integrated with verification evidence\",\n                        )\n                        .map_err(RuntimeError::agent)?;\n                        let _ = events.send(RuntimeEvent::Plan(\n                            crate::production_orchestrator::projection(ledger),\n                        ));\n                    }\n                    Some(evidence)\n                }\n                Err(error) => {\n                    if let Some(ledger) = execution_ledger.as_mut() {\n                        let _ = crate::production_orchestrator::fail_kinds(\n                            ledger,\n                            &execution_plan,\n                            &[medusa_multi_agent_scheduler::TaskKind::Implementation],\n                            &error,\n                        );\n                        let _ = events.send(RuntimeEvent::Plan(\n                            crate::production_orchestrator::projection(ledger),\n                        ));\n                    }\n                    return Err(RuntimeError::agent(error));\n                }\n            }\n        } else {\n            None\n        };\n",
)
replace_once(
    runtime,
    "    let mut updates = UpdateState::new();\n    if !session.plan.is_empty() {\n        let _ = events.send(RuntimeEvent::Plan(session.plan.clone()));\n    }\n",
    "    if coordinated {\n        if let Some(ledger) = execution_ledger.as_mut() {\n            crate::production_orchestrator::begin_kinds(\n                ledger,\n                &execution_plan,\n                &[medusa_multi_agent_scheduler::TaskKind::Review],\n                \"parent-review\",\n            )\n            .map_err(RuntimeError::agent)?;\n            let _ = events.send(RuntimeEvent::Plan(\n                crate::production_orchestrator::projection(ledger),\n            ));\n        }\n    }\n    let mut updates = UpdateState::new();\n    if coordinated {\n        updates.suppress_model_plan();\n    }\n    if !coordinated && !session.plan.is_empty() {\n        let _ = events.send(RuntimeEvent::Plan(session.plan.clone()));\n    }\n",
)
replace_once(
    runtime,
    "            if cancel_requested(cancel, submission) {\n                return Ok(RuntimeEvent::Cancelled);\n            }\n",
    "            if cancel_requested(cancel, submission) {\n                if let Some(ledger) = execution_ledger.as_mut() {\n                    let _ = ledger.cancel_remaining(\"runtime cancellation requested\");\n                    let projected = crate::production_orchestrator::projection(ledger);\n                    session.plan = projected.clone();\n                    let _ = events.send(RuntimeEvent::Plan(projected));\n                }\n                return Ok(RuntimeEvent::Cancelled);\n            }\n",
)
replace_once(
    runtime,
    "    let mut result = result;\n    let mut verified = matches!(&result, Ok(RuntimeEvent::Completed { .. }));\n    if execution_plan.mode == crate::production_orchestrator::ExecutionMode::Orchestrated\n        && matches!(&result, Ok(RuntimeEvent::Completed { .. }))\n    {\n        match crate::multi_agent_coordinator::verify_repository(\n            &state.repo,\n            &execution_plan,\n            events,\n        ) {\n            Ok(_) => verified = true,\n            Err(error) => result = Err(RuntimeError::agent(error)),\n        }\n    }\n",
    "    let mut result = result;\n    if coordinated {\n        if let Some(ledger) = execution_ledger.as_mut() {\n            match &result {\n                Ok(RuntimeEvent::Completed { .. } | RuntimeEvent::TurnFinished) => {\n                    crate::production_orchestrator::succeed_kinds(\n                        ledger,\n                        &execution_plan,\n                        &[medusa_multi_agent_scheduler::TaskKind::Review],\n                        \"parent review completed from durable execution evidence\",\n                    )\n                    .map_err(RuntimeError::agent)?;\n                }\n                Ok(RuntimeEvent::Cancelled) => {\n                    let _ = ledger.cancel_remaining(\"runtime cancellation completed\");\n                }\n                Err(error) => {\n                    let _ = crate::production_orchestrator::fail_kinds(\n                        ledger,\n                        &execution_plan,\n                        &[medusa_multi_agent_scheduler::TaskKind::Review],\n                        &error.to_string(),\n                    );\n                }\n                _ => {}\n            }\n            let projected = crate::production_orchestrator::projection(ledger);\n            session.plan = projected.clone();\n            let _ = events.send(RuntimeEvent::Plan(projected));\n        }\n    }\n    let terminal_turn = matches!(\n        &result,\n        Ok(RuntimeEvent::Completed { .. } | RuntimeEvent::TurnFinished)\n    );\n    let mut verified = terminal_turn && !crate::production_orchestrator::requires_mutation(&execution_plan);\n    if execution_plan.mode == crate::production_orchestrator::ExecutionMode::Orchestrated\n        && terminal_turn\n        && execution_plan\n            .planning\n            .task(medusa_multi_agent_scheduler::TaskKind::Verification)\n            .is_some()\n    {\n        if let Some(ledger) = execution_ledger.as_mut() {\n            crate::production_orchestrator::begin_kinds(\n                ledger,\n                &execution_plan,\n                &[medusa_multi_agent_scheduler::TaskKind::Verification],\n                \"repository-verification\",\n            )\n            .map_err(RuntimeError::agent)?;\n            let _ = events.send(RuntimeEvent::Plan(\n                crate::production_orchestrator::projection(ledger),\n            ));\n        }\n        match crate::multi_agent_coordinator::verify_repository(\n            &state.repo,\n            &execution_plan,\n            events,\n        ) {\n            Ok(evidence) => {\n                verified = true;\n                if let Some(ledger) = execution_ledger.as_mut() {\n                    crate::production_orchestrator::succeed_kinds(\n                        ledger,\n                        &execution_plan,\n                        &[medusa_multi_agent_scheduler::TaskKind::Verification],\n                        &evidence.join(\" | \"),\n                    )\n                    .map_err(RuntimeError::agent)?;\n                }\n            }\n            Err(error) => {\n                if let Some(ledger) = execution_ledger.as_mut() {\n                    let _ = crate::production_orchestrator::fail_kinds(\n                        ledger,\n                        &execution_plan,\n                        &[medusa_multi_agent_scheduler::TaskKind::Verification],\n                        &error,\n                    );\n                }\n                result = Err(RuntimeError::agent(error));\n            }\n        }\n        if let Some(ledger) = execution_ledger.as_ref() {\n            let projected = crate::production_orchestrator::projection(ledger);\n            session.plan = projected.clone();\n            let _ = events.send(RuntimeEvent::Plan(projected));\n        }\n    }\n",
)

support = Path("crates/medusa-runtime/src/support.rs")
replace_once(
    support,
    "    pub(super) current_context_tokens: u64,\n}",
    "    pub(super) current_context_tokens: u64,\n    suppress_model_plan: bool,\n}",
)
replace_once(
    support,
    "            current_context_tokens: 0,\n        }\n    }\n}",
    "            current_context_tokens: 0,\n            suppress_model_plan: false,\n        }\n    }\n\n    pub(super) fn suppress_model_plan(&mut self) {\n        self.suppress_model_plan = true;\n    }\n}",
)
replace_once(
    support,
    "        AgentUpdate::Plan(steps) => {\n            let _ = events.send(RuntimeEvent::Plan(steps.clone()));\n        }",
    "        AgentUpdate::Plan(steps) => {\n            if !state.suppress_model_plan {\n                let _ = events.send(RuntimeEvent::Plan(steps.clone()));\n            }\n        }",
)

index = Path("docs/architecture/INDEX.md")
replace_once(
    index,
    "- Decision: [`decisions/0003-truthful-capability-plugin-registry.md`](decisions/0003-truthful-capability-plugin-registry.md)\n",
    "- Decision: [`decisions/0003-truthful-capability-plugin-registry.md`](decisions/0003-truthful-capability-plugin-registry.md)\n- Decision: [`decisions/0004-authoritative-durable-scheduler.md`](decisions/0004-authoritative-durable-scheduler.md)\n",
)
