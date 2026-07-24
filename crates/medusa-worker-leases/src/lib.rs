//! Durable worker leases, heartbeat expiry, and deterministic reassignment.

use std::collections::{BTreeMap, BTreeSet};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkerLease {
    pub worker_id: String,
    pub task_id: String,
    pub lease_epoch: u64,
    pub acquired_at_ms: u64,
    pub heartbeat_at_ms: u64,
    pub timeout_ms: u64,
    pub fingerprint: String,
}

impl WorkerLease {
    pub fn acquire(worker_id: impl Into<String>, task_id: impl Into<String>, lease_epoch: u64, now_ms: u64, timeout_ms: u64) -> Result<Self, &'static str> {
        let worker_id = worker_id.into();
        let task_id = task_id.into();
        if worker_id.trim().is_empty() || task_id.trim().is_empty() { return Err("worker and task identifiers cannot be empty"); }
        if timeout_ms == 0 { return Err("lease timeout must be positive"); }
        let fingerprint = hash(&(worker_id.as_str(), task_id.as_str(), lease_epoch, now_ms, now_ms, timeout_ms));
        Ok(Self { worker_id, task_id, lease_epoch, acquired_at_ms: now_ms, heartbeat_at_ms: now_ms, timeout_ms, fingerprint })
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        let rebuilt = Self::acquire(self.worker_id.clone(), self.task_id.clone(), self.lease_epoch, self.acquired_at_ms, self.timeout_ms)?;
        let expected = hash(&(rebuilt.worker_id.as_str(), rebuilt.task_id.as_str(), rebuilt.lease_epoch, rebuilt.acquired_at_ms, self.heartbeat_at_ms, rebuilt.timeout_ms));
        if expected != self.fingerprint { return Err("lease fingerprint does not match contents"); }
        if self.heartbeat_at_ms < self.acquired_at_ms { return Err("heartbeat cannot predate acquisition"); }
        Ok(())
    }

    pub fn heartbeat(&mut self, now_ms: u64) -> Result<(), &'static str> {
        self.validate()?;
        if now_ms < self.heartbeat_at_ms { return Err("heartbeat time cannot move backwards"); }
        self.heartbeat_at_ms = now_ms;
        self.fingerprint = hash(&(self.worker_id.as_str(), self.task_id.as_str(), self.lease_epoch, self.acquired_at_ms, self.heartbeat_at_ms, self.timeout_ms));
        Ok(())
    }

    pub fn expired(&self, now_ms: u64) -> Result<bool, &'static str> {
        self.validate()?;
        Ok(now_ms.saturating_sub(self.heartbeat_at_ms) > self.timeout_ms)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeaseRegistry {
    pub leases: Vec<WorkerLease>,
    pub fingerprint: String,
}

impl LeaseRegistry {
    pub fn record(leases: impl IntoIterator<Item = WorkerLease>) -> Result<Self, &'static str> {
        let mut by_task = BTreeMap::new();
        let mut worker_tasks = BTreeSet::new();
        for lease in leases {
            lease.validate()?;
            if by_task.insert(lease.task_id.clone(), lease.clone()).is_some() { return Err("only one active lease is allowed per task"); }
            if !worker_tasks.insert((lease.worker_id.clone(), lease.task_id.clone())) { return Err("duplicate worker lease"); }
        }
        let leases = by_task.into_values().collect::<Vec<_>>();
        let fingerprint = hash(&leases);
        Ok(Self { leases, fingerprint })
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        let rebuilt = Self::record(self.leases.clone())?;
        if rebuilt.fingerprint != self.fingerprint { return Err("registry fingerprint does not match contents"); }
        Ok(())
    }

    pub fn expired_tasks(&self, now_ms: u64) -> Result<Vec<String>, &'static str> {
        self.validate()?;
        let mut tasks = self.leases.iter().filter_map(|lease| lease.expired(now_ms).ok().filter(|expired| *expired).map(|_| lease.task_id.clone())).collect::<Vec<_>>();
        tasks.sort();
        Ok(tasks)
    }

    pub fn next_epoch(&self, task_id: &str) -> Result<u64, &'static str> {
        self.validate()?;
        Ok(self.leases.iter().filter(|lease| lease.task_id == task_id).map(|lease| lease.lease_epoch).max().unwrap_or(0) + 1)
    }
}

pub fn deterministic_reassignment(task_id: &str, previous_worker: &str, candidates: impl IntoIterator<Item = String>) -> Result<String, &'static str> {
    if task_id.trim().is_empty() { return Err("task identifier cannot be empty"); }
    let mut candidates = candidates.into_iter().filter(|worker| worker != previous_worker).collect::<BTreeSet<_>>();
    candidates.pop_first().ok_or("no replacement worker is available")
}

fn hash<T: Serialize>(value: &T) -> String {
    hex::encode(Sha256::digest(serde_json::to_vec(value).expect("lease state serializes")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_extends_a_lease() {
        let mut lease = WorkerLease::acquire("worker-a", "task-1", 1, 100, 50).unwrap();
        assert!(!lease.expired(140).unwrap());
        lease.heartbeat(140).unwrap();
        assert!(!lease.expired(180).unwrap());
        assert!(lease.expired(191).unwrap());
    }

    #[test]
    fn registry_reports_expired_tasks_deterministically() {
        let a = WorkerLease::acquire("a", "task-b", 1, 0, 10).unwrap();
        let b = WorkerLease::acquire("b", "task-a", 1, 0, 100).unwrap();
        let registry = LeaseRegistry::record([a, b]).unwrap();
        assert_eq!(registry.expired_tasks(20).unwrap(), vec!["task-b"]);
    }

    #[test]
    fn reassignment_excludes_failed_worker_and_is_stable() {
        assert_eq!(deterministic_reassignment("task", "b", ["c".into(), "a".into(), "b".into()]).unwrap(), "a");
    }

    #[test]
    fn tampering_is_rejected() {
        let mut lease = WorkerLease::acquire("a", "t", 1, 0, 10).unwrap();
        lease.timeout_ms = 99;
        assert!(lease.validate().is_err());
    }
}
