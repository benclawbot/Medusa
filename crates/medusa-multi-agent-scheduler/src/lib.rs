//! Deterministic dependency-aware scheduling for parallel Medusa workers.

use std::collections::{BTreeMap, BTreeSet};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
        let ready = remaining.iter().filter(|id| {
            tasks[*id].dependencies.iter().all(|dependency| complete.contains(dependency))
        }).cloned().collect::<Vec<_>>();
        if ready.is_empty() { return Err("task graph cannot make progress"); }

        let mut capacity = workers.values().filter(|worker| worker.healthy)
            .map(|worker| (worker.id.clone(), worker.capacity)).collect::<BTreeMap<_, _>>();
        let mut paths = BTreeSet::new();
        let mut wave = Vec::new();

        for id in ready {
            let task = &tasks[&id];
            if task.write_paths.iter().any(|path| paths.contains(path)) { continue; }
            let worker = workers.values().find(|worker| {
                worker.healthy
                    && capacity.get(&worker.id).copied().unwrap_or(0) > 0
                    && task.capabilities.iter().all(|capability| worker.capabilities.binary_search(capability).is_ok())
            });
            if let Some(worker) = worker {
                *capacity.get_mut(&worker.id).expect("worker capacity exists") -= 1;
                paths.extend(task.write_paths.iter().cloned());
                wave.push(Assignment { task_id: id, worker_id: worker.id.clone(), speculative: task.speculative });
            }
        }
        if wave.is_empty() { return Err("no healthy capable worker can execute a ready task"); }
        wave.sort_by(|a, b| a.task_id.cmp(&b.task_id).then(a.worker_id.cmp(&b.worker_id)));
        for assignment in &wave {
            remaining.remove(&assignment.task_id);
            complete.insert(assignment.task_id.clone());
        }
        waves.push(wave);
    }

    let fingerprint = hash(&waves);
    Ok(Schedule { waves, fingerprint })
}

pub fn overlapping_paths(tasks: &[Task]) -> Result<BTreeMap<String, Vec<String>>, &'static str> {
    let tasks = canonical_tasks(tasks.to_vec())?;
    let mut paths: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for task in tasks.values() {
        for path in &task.write_paths { paths.entry(path.clone()).or_default().push(task.id.clone()); }
    }
    paths.retain(|_, ids| ids.len() > 1);
    Ok(paths)
}

pub fn replacement(task: &Task, unavailable: &str, workers: &[Worker]) -> Result<String, &'static str> {
    validate_task(task)?;
    let workers = canonical_workers(workers.to_vec())?;
    workers.values().find(|worker| {
        worker.id != unavailable && worker.healthy && worker.capacity > 0
            && task.capabilities.iter().all(|capability| worker.capabilities.binary_search(capability).is_ok())
    }).map(|worker| worker.id.clone()).ok_or("no replacement worker is available")
}

pub fn obsolete_speculation(assignments: &[Assignment], invalidated: &[String]) -> Vec<String> {
    let invalidated = invalidated.iter().collect::<BTreeSet<_>>();
    let mut result = assignments.iter()
        .filter(|assignment| assignment.speculative && invalidated.contains(&assignment.task_id))
        .map(|assignment| assignment.task_id.clone()).collect::<Vec<_>>();
    result.sort();
    result.dedup();
    result
}

fn canonical_tasks(tasks: Vec<Task>) -> Result<BTreeMap<String, Task>, &'static str> {
    if tasks.is_empty() { return Err("at least one task is required"); }
    let mut result = BTreeMap::new();
    for mut task in tasks {
        task.dependencies.sort();
        task.capabilities.sort();
        task.write_paths.sort();
        validate_task(&task)?;
        if result.insert(task.id.clone(), task).is_some() { return Err("task identifiers must be unique"); }
    }
    Ok(result)
}

fn canonical_workers(workers: Vec<Worker>) -> Result<BTreeMap<String, Worker>, &'static str> {
    if workers.is_empty() { return Err("at least one worker is required"); }
    let mut result = BTreeMap::new();
    for mut worker in workers {
        worker.capabilities.sort();
        if worker.id.trim().is_empty() || worker.capacity == 0 { return Err("worker identifier and capacity must be valid"); }
        if !unique(&worker.capabilities) { return Err("worker capabilities must be unique"); }
        if result.insert(worker.id.clone(), worker).is_some() { return Err("worker identifiers must be unique"); }
    }
    Ok(result)
}

fn validate_task(task: &Task) -> Result<(), &'static str> {
    if task.id.trim().is_empty() { return Err("task identifier cannot be empty"); }
    if !unique(&task.dependencies) || !unique(&task.capabilities) || !unique(&task.write_paths) { return Err("task lists must contain unique values"); }
    if task.dependencies.contains(&task.id) { return Err("task cannot depend on itself"); }
    if task.write_paths.iter().any(|path| path.is_empty() || path.starts_with('/') || path.split('/').any(|part| part == "..")) {
        return Err("write paths must be workspace relative");
    }
    Ok(())
}

fn validate_graph(tasks: &BTreeMap<String, Task>) -> Result<(), &'static str> {
    for task in tasks.values() {
        if task.dependencies.iter().any(|dependency| !tasks.contains_key(dependency)) { return Err("task dependency does not exist"); }
    }
    let mut done = BTreeSet::new();
    loop {
        let before = done.len();
        for task in tasks.values() {
            if task.dependencies.iter().all(|dependency| done.contains(dependency)) { done.insert(task.id.clone()); }
        }
        if done.len() == tasks.len() { return Ok(()); }
        if done.len() == before { return Err("task dependency graph contains a cycle"); }
    }
}

fn unique(values: &[String]) -> bool { values.iter().collect::<BTreeSet<_>>().len() == values.len() }
fn hash<T: Serialize>(value: &T) -> String {
    hex::encode(Sha256::digest(serde_json::to_vec(value).expect("scheduler state serializes")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, dependencies: &[&str], path: &str) -> Task {
        Task { id: id.into(), dependencies: dependencies.iter().map(|v| (*v).into()).collect(), capabilities: vec!["rust".into()], write_paths: vec![path.into()], speculative: false }
    }
    fn worker(id: &str) -> Worker { Worker { id: id.into(), capabilities: vec!["rust".into()], healthy: true, capacity: 1 } }

    #[test]
    fn independent_tasks_run_in_parallel() {
        let result = schedule(vec![task("a", &[], "a.rs"), task("b", &[], "b.rs")], vec![worker("one"), worker("two")]).unwrap();
        assert_eq!(result.waves.len(), 1);
        assert_eq!(result.waves[0].len(), 2);
    }

    #[test]
    fn dependencies_and_path_conflicts_create_new_waves() {
        let dependent = schedule(vec![task("a", &[], "a.rs"), task("b", &["a"], "b.rs")], vec![worker("one"), worker("two")]).unwrap();
        assert_eq!(dependent.waves.len(), 2);
        let conflict = schedule(vec![task("a", &[], "same.rs"), task("b", &[], "same.rs")], vec![worker("one"), worker("two")]).unwrap();
        assert_eq!(conflict.waves.len(), 2);
    }

    #[test]
    fn scheduling_is_deterministic_and_supports_reassignment() {
        let tasks = vec![task("a", &[], "a.rs"), task("b", &[], "b.rs")];
        let workers = vec![worker("one"), worker("two")];
        assert_eq!(schedule(tasks.clone(), workers.clone()).unwrap(), schedule(tasks.into_iter().rev().collect(), workers.into_iter().rev().collect()).unwrap());
        assert_eq!(replacement(&task("a", &[], "a.rs"), "one", &[worker("one"), worker("two")]).unwrap(), "two");
    }
}
