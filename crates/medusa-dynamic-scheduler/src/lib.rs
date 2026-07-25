//! Feedback-driven scheduling for Medusa's parallel worker runtime.
//!
//! The static scheduler remains responsible for deterministic initial planning.
//! This crate owns execution-time state: dispatch, completion, retry, worker
//! quarantine, dependency release, and durable state validation.

use std::collections::{BTreeMap, BTreeSet};

use medusa_multi_agent_scheduler::{Assignment, Task, Worker};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum TaskState {
    Pending,
    Running { worker_id: String, attempt: u32 },
    Succeeded,
    Failed { attempts: u32, reason: String },
}

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
        if tasks.is_empty() || workers.is_empty() || max_attempts == 0 {
            return Err("tasks, workers, and a non-zero retry limit are required");
        }

        let mut task_map = BTreeMap::new();
        for mut task in tasks {
            task.dependencies.sort();
            task.dependencies.dedup();
            task.capabilities.sort();
            task.capabilities.dedup();
            task.write_paths.sort();
            task.write_paths.dedup();
            validate_task(&task)?;
            if task_map.insert(task.id.clone(), task).is_some() {
                return Err("task identifiers must be unique");
            }
        }
        validate_graph(&task_map)?;

        let mut worker_map = BTreeMap::new();
        for mut worker in workers {
            worker.capabilities.sort();
            worker.capabilities.dedup();
            if worker.id.trim().is_empty() || worker.capacity == 0 {
                return Err("worker identifier and capacity must be valid");
            }
            if worker_map.insert(worker.id.clone(), worker).is_some() {
                return Err("worker identifiers must be unique");
            }
        }

        let states = task_map
            .keys()
            .map(|id| (id.clone(), TaskState::Pending))
            .collect();
        let mut schedule = Self {
            tasks: task_map,
            workers: worker_map,
            states,
            max_attempts,
            fingerprint: String::new(),
        };
        schedule.refresh()?;
        Ok(schedule)
    }

    pub fn dispatch_ready(&mut self) -> Result<Vec<Assignment>, &'static str> {
        self.validate()?;
        let succeeded = self
            .states
            .iter()
            .filter_map(|(id, state)| matches!(state, TaskState::Succeeded).then_some(id.clone()))
            .collect::<BTreeSet<_>>();
        let mut capacity = self.available_capacity();
        let mut claimed_paths = self.running_write_paths();
        let mut assignments = Vec::new();

        for (task_id, task) in &self.tasks {
            if !matches!(self.states.get(task_id), Some(TaskState::Pending)) {
                continue;
            }
            if !task.dependencies.iter().all(|dependency| succeeded.contains(dependency)) {
                continue;
            }
            if task.write_paths.iter().any(|path| claimed_paths.contains(path)) {
                continue;
            }

            let worker_id = self.workers.values().find_map(|worker| {
                let remaining = capacity.get(&worker.id).copied().unwrap_or(0);
                (worker.healthy
                    && remaining > 0
                    && task.capabilities.iter().all(|capability| {
                        worker.capabilities.binary_search(capability).is_ok()
                    }))
                .then(|| worker.id.clone())
            });

            let Some(worker_id) = worker_id else {
                continue;
            };
            let attempt = self.attempts_for(task_id).saturating_add(1);
            self.states.insert(
                task_id.clone(),
                TaskState::Running {
                    worker_id: worker_id.clone(),
                    attempt,
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

        self.refresh()?;
        Ok(assignments)
    }

    pub fn complete(&mut self, task_id: &str, worker_id: &str) -> Result<(), &'static str> {
        self.validate_running(task_id, worker_id)?;
        self.states.insert(task_id.to_owned(), TaskState::Succeeded);
        self.refresh()
    }

    pub fn fail(
        &mut self,
        task_id: &str,
        worker_id: &str,
        reason: impl Into<String>,
        retryable: bool,
    ) -> Result<(), &'static str> {
        let attempt = self.validate_running(task_id, worker_id)?;
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err("failure reason cannot be empty");
        }
        let next = if retryable && attempt < self.max_attempts {
            TaskState::Pending
        } else {
            TaskState::Failed {
                attempts: attempt,
                reason,
            }
        };
        self.states.insert(task_id.to_owned(), next);
        self.refresh()
    }

    pub fn set_worker_health(&mut self, worker_id: &str, healthy: bool) -> Result<(), &'static str> {
        let worker = self
            .workers
            .get_mut(worker_id)
            .ok_or("worker does not exist")?;
        worker.healthy = healthy;
        if !healthy {
            let interrupted = self
                .states
                .iter()
                .filter_map(|(task_id, state)| match state {
                    TaskState::Running {
                        worker_id: assigned,
                        ..
                    } if assigned == worker_id => Some(task_id.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            for task_id in interrupted {
                self.states.insert(task_id, TaskState::Pending);
            }
        }
        self.refresh()
    }

    pub fn state(&self, task_id: &str) -> Option<&TaskState> {
        self.states.get(task_id)
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
                (matches!(self.states.get(id), Some(TaskState::Pending))
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
        for task_id in self.tasks.keys() {
            if !self.states.contains_key(task_id) {
                return Err("task state is missing");
            }
        }
        let expected = fingerprint(&(
            &self.tasks,
            &self.workers,
            &self.states,
            self.max_attempts,
        ))?;
        if expected != self.fingerprint {
            return Err("dynamic schedule fingerprint does not match its contents");
        }
        Ok(())
    }

    fn attempts_for(&self, task_id: &str) -> u32 {
        match self.states.get(task_id) {
            Some(TaskState::Running { attempt, .. }) => *attempt,
            Some(TaskState::Failed { attempts, .. }) => *attempts,
            _ => 0,
        }
    }

    fn validate_running(&self, task_id: &str, worker_id: &str) -> Result<u32, &'static str> {
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

    fn available_capacity(&self) -> BTreeMap<String, u16> {
        let mut capacity = self
            .workers
            .values()
            .map(|worker| (worker.id.clone(), worker.capacity))
            .collect::<BTreeMap<_, _>>();
        for state in self.states.values() {
            if let TaskState::Running { worker_id, .. } = state {
                if let Some(value) = capacity.get_mut(worker_id) {
                    *value = value.saturating_sub(1);
                }
            }
        }
        capacity
    }

    fn running_write_paths(&self) -> BTreeSet<String> {
        self.states
            .iter()
            .filter_map(|(task_id, state)| {
                matches!(state, TaskState::Running { .. }).then_some(&self.tasks[task_id])
            })
            .flat_map(|task| task.write_paths.iter().cloned())
            .collect()
    }

    fn refresh(&mut self) -> Result<(), &'static str> {
        self.fingerprint = fingerprint(&(
            &self.tasks,
            &self.workers,
            &self.states,
            self.max_attempts,
        ))?;
        Ok(())
    }
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
    let mut complete = BTreeSet::new();
    loop {
        let before = complete.len();
        for task in tasks.values() {
            if task
                .dependencies
                .iter()
                .all(|dependency| complete.contains(dependency))
            {
                complete.insert(task.id.clone());
            }
        }
        if complete.len() == tasks.len() {
            return Ok(());
        }
        if complete.len() == before {
            return Err("task dependency graph contains a cycle");
        }
    }
}

fn fingerprint<T: Serialize>(value: &T) -> Result<String, &'static str> {
    let bytes = serde_json::to_vec(value).map_err(|_| "scheduler serialization failed")?;
    Ok(hex::encode(Sha256::digest(bytes)))
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
    fn completion_releases_dependent_work() {
        let mut schedule = DynamicSchedule::new(
            vec![task("plan", &[], "plan.md"), task("code", &["plan"], "src/lib.rs")],
            vec![worker("one")],
            2,
        )
        .unwrap();
        let first = schedule.dispatch_ready().unwrap();
        assert_eq!(first[0].task_id, "plan");
        schedule.complete("plan", "one").unwrap();
        let second = schedule.dispatch_ready().unwrap();
        assert_eq!(second[0].task_id, "code");
    }

    #[test]
    fn unhealthy_worker_requeues_running_work() {
        let mut schedule = DynamicSchedule::new(
            vec![task("code", &[], "src/lib.rs")],
            vec![worker("one"), worker("two")],
            2,
        )
        .unwrap();
        let first = schedule.dispatch_ready().unwrap();
        assert_eq!(first[0].worker_id, "one");
        schedule.set_worker_health("one", false).unwrap();
        let second = schedule.dispatch_ready().unwrap();
        assert_eq!(second[0].worker_id, "two");
    }

    #[test]
    fn retry_limit_creates_terminal_failure_and_blocks_dependents() {
        let mut schedule = DynamicSchedule::new(
            vec![task("code", &[], "src/lib.rs"), task("test", &["code"], "tests/a.rs")],
            vec![worker("one")],
            1,
        )
        .unwrap();
        schedule.dispatch_ready().unwrap();
        schedule.fail("code", "one", "compiler error", true).unwrap();
        assert!(schedule.has_terminal_failure());
        assert_eq!(schedule.blocked_tasks(), vec!["test"]);
    }

    #[test]
    fn running_write_paths_prevent_conflicting_dispatch() {
        let mut schedule = DynamicSchedule::new(
            vec![task("a", &[], "src/lib.rs"), task("b", &[], "src/lib.rs")],
            vec![worker("one"), worker("two")],
            2,
        )
        .unwrap();
        assert_eq!(schedule.dispatch_ready().unwrap().len(), 1);
        assert!(schedule.dispatch_ready().unwrap().is_empty());
    }
}
