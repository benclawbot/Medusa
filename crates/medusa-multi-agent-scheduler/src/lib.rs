//! Deterministic dependency-aware scheduling for parallel Medusa workers.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

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
            .filter(|id| tasks[*id].dependencies.iter().all(|dependency| complete.contains(dependency)))
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
        wave.sort_by(|a, b| a.task_id.cmp(&b.task_id).then(a.worker_id.cmp(&b.worker_id)));
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

pub fn replacement(task: &Task, unavailable: &str, workers: &[Worker]) -> Result<String, &'static str> {
    validate_task(task)?;
    canonical_workers(workers.to_vec())?
        .values()
        .find(|worker| worker.id != unavailable && worker.healthy && worker.capacity > 0 && supports(worker, task))
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
///
/// It releases dependencies only after observed success, preserves retry counts,
/// requeues work from unhealthy workers, enforces worker capacity, and prevents
/// concurrent writes to the same repository-relative path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DynamicSchedule {
    tasks: BTreeMap<String, Task>,
    workers: BTreeMap<String, Worker>,
    states: BTreeMap<String, TaskState>,
    max_attempts: u32,
    fingerprint: String,
}

impl DynamicSchedule {
    pub fn new(tasks: Vec<Task>, workers: Vec<Worker>, max_attempts: u32) -> Result<Self, &'static str> {
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
        let mut value = Self {
            tasks,
            workers,
            states,
            max_attempts,
            fingerprint: String::new(),
        };
        value.refresh();
        Ok(value)
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
            if !task.dependencies.iter().all(|dependency| succeeded.contains(dependency))
                || task.write_paths.iter().any(|path| claimed_paths.contains(path))
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

    pub fn set_worker_health(&mut self, worker_id: &str, healthy: bool) -> Result<(), &'static str> {
        self.workers.get_mut(worker_id).ok_or("worker does not exist")?.healthy = healthy;
        if !healthy {
            let interrupted = self
                .states
                .iter()
                .filter_map(|(task_id, state)| match state {
                    TaskState::Running { worker_id: assigned, attempt } if assigned == worker_id => {
                        Some((task_id.clone(), *attempt))
                    }
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

    pub fn is_complete(&self) -> bool {
        self.states.values().all(|state| matches!(state, TaskState::Succeeded))
    }

    pub fn has_terminal_failure(&self) -> bool {
        self.states.values().any(|state| matches!(state, TaskState::Failed { .. }))
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
                    && task.dependencies.iter().any(|dependency| failed.contains(dependency)))
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
            Some(TaskState::Running { worker_id: assigned, attempt }) if assigned == worker_id => Ok(*attempt),
            Some(TaskState::Running { .. }) => Err("task is owned by a different worker"),
            Some(_) => Err("task is not running"),
            None => Err("task does not exist"),
        }
    }

    fn running_paths(&self) -> BTreeSet<String> {
        self.states
            .iter()
            .filter_map(|(task_id, state)| matches!(state, TaskState::Running { .. }).then_some(&self.tasks[task_id]))
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
        if task.dependencies.iter().any(|dependency| !tasks.contains_key(dependency)) {
            return Err("task dependency does not exist");
        }
    }
    let mut done = BTreeSet::new();
    loop {
        let before = done.len();
        for task in tasks.values() {
            if task.dependencies.iter().all(|dependency| done.contains(dependency)) {
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
            schedule(tasks.into_iter().rev().collect(), workers.into_iter().rev().collect()).unwrap()
        );
        assert_eq!(replacement(&task("a", &[], "a.rs"), "one", &[worker("one"), worker("two")]).unwrap(), "two");
    }

    #[test]
    fn dynamic_completion_releases_dependencies() {
        let mut runtime = DynamicSchedule::new(
            vec![task("plan", &[], "plan.md"), task("code", &["plan"], "src/lib.rs")],
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
        assert_eq!(runtime.state("code"), Some(&TaskState::Running { worker_id: "two".into(), attempt: 2 }));
    }

    #[test]
    fn dynamic_retry_limit_blocks_dependents() {
        let mut runtime = DynamicSchedule::new(
            vec![task("code", &[], "src/lib.rs"), task("test", &["code"], "tests/a.rs")],
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
}
