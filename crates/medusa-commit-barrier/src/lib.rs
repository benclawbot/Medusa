//! Deterministic two-phase commit coordination for Medusa worker transactions.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Vote {
    Prepared,
    Abort(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParticipantVote {
    pub worker_id: String,
    pub task_id: String,
    pub lease_epoch: u64,
    pub transaction_fingerprint: String,
    pub vote: Vote,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum BarrierDecision {
    Commit,
    Abort,
    Pending,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommitBarrier {
    pub barrier_id: String,
    pub expected_tasks: Vec<String>,
    pub votes: Vec<ParticipantVote>,
    pub deadline_ms: u64,
    pub decision: BarrierDecision,
    pub fingerprint: String,
}

impl CommitBarrier {
    pub fn create(
        barrier_id: impl Into<String>,
        expected_tasks: Vec<String>,
        deadline_ms: u64,
    ) -> Result<Self, &'static str> {
        let barrier_id = barrier_id.into();
        if barrier_id.trim().is_empty() || deadline_ms == 0 {
            return Err("barrier identifier and deadline must be valid");
        }
        let expected_tasks = canonical_unique(expected_tasks)?;
        if expected_tasks.is_empty() {
            return Err("at least one participant is required");
        }
        let mut barrier = Self {
            barrier_id,
            expected_tasks,
            votes: Vec::new(),
            deadline_ms,
            decision: BarrierDecision::Pending,
            fingerprint: String::new(),
        };
        barrier.refresh_fingerprint();
        Ok(barrier)
    }

    pub fn record_vote(&mut self, vote: ParticipantVote) -> Result<(), &'static str> {
        self.validate()?;
        validate_vote(&vote)?;
        if self.decision != BarrierDecision::Pending {
            return Err("finalized barriers cannot accept votes");
        }
        if !self.expected_tasks.contains(&vote.task_id) {
            return Err("vote task is not a barrier participant");
        }
        if self.votes.iter().any(|existing| existing.task_id == vote.task_id) {
            return Err("a task may vote only once");
        }
        self.votes.push(vote);
        self.votes.sort_by(|a, b| a.task_id.cmp(&b.task_id).then(a.worker_id.cmp(&b.worker_id)));
        self.decision = if self.votes.iter().any(|item| matches!(item.vote, Vote::Abort(_))) {
            BarrierDecision::Abort
        } else if self.votes.len() == self.expected_tasks.len() {
            BarrierDecision::Commit
        } else {
            BarrierDecision::Pending
        };
        self.refresh_fingerprint();
        Ok(())
    }

    pub fn expire(&mut self, now_ms: u64) -> Result<(), &'static str> {
        self.validate()?;
        if self.decision == BarrierDecision::Pending && now_ms >= self.deadline_ms {
            self.decision = BarrierDecision::Abort;
            self.refresh_fingerprint();
        }
        Ok(())
    }

    pub fn validate_active_leases(
        &mut self,
        active_epochs: &BTreeMap<String, u64>,
    ) -> Result<(), &'static str> {
        self.validate()?;
        if self.decision != BarrierDecision::Pending {
            return Ok(());
        }
        let stale = self.votes.iter().any(|vote| {
            active_epochs.get(&vote.task_id).copied() != Some(vote.lease_epoch)
        });
        if stale {
            self.decision = BarrierDecision::Abort;
            self.refresh_fingerprint();
        }
        Ok(())
    }

    pub fn prepared_transactions(&self) -> Result<Vec<String>, &'static str> {
        self.validate()?;
        if self.decision != BarrierDecision::Commit {
            return Err("transactions may be promoted only after commit");
        }
        Ok(self.votes.iter().map(|vote| vote.transaction_fingerprint.clone()).collect())
    }

    pub fn rollback_transactions(&self) -> Result<Vec<String>, &'static str> {
        self.validate()?;
        if self.decision != BarrierDecision::Abort {
            return Err("rollback is available only after abort");
        }
        Ok(self.votes.iter().map(|vote| vote.transaction_fingerprint.clone()).collect())
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.barrier_id.trim().is_empty() || self.deadline_ms == 0 {
            return Err("barrier identifier and deadline must be valid");
        }
        if canonical_unique(self.expected_tasks.clone())? != self.expected_tasks {
            return Err("barrier participants must be canonical and unique");
        }
        let mut task_ids = BTreeSet::new();
        for vote in &self.votes {
            validate_vote(vote)?;
            if !self.expected_tasks.contains(&vote.task_id) || !task_ids.insert(&vote.task_id) {
                return Err("barrier contains invalid or duplicate votes");
            }
        }
        let expected = fingerprint(&(
            &self.barrier_id,
            &self.expected_tasks,
            &self.votes,
            self.deadline_ms,
            &self.decision,
        ));
        if expected != self.fingerprint {
            return Err("barrier fingerprint does not match its contents");
        }
        Ok(())
    }

    fn refresh_fingerprint(&mut self) {
        self.fingerprint = fingerprint(&(
            &self.barrier_id,
            &self.expected_tasks,
            &self.votes,
            self.deadline_ms,
            &self.decision,
        ));
    }
}

fn validate_vote(vote: &ParticipantVote) -> Result<(), &'static str> {
    if vote.worker_id.trim().is_empty() || vote.task_id.trim().is_empty() || vote.lease_epoch == 0 {
        return Err("vote identifiers and lease epoch must be valid");
    }
    validate_digest(&vote.transaction_fingerprint)
}

fn canonical_unique(mut values: Vec<String>) -> Result<Vec<String>, &'static str> {
    if values.iter().any(|value| value.trim().is_empty()) {
        return Err("identifiers cannot be empty");
    }
    values.sort();
    let original_len = values.len();
    values.dedup();
    if values.len() != original_len {
        return Err("identifiers must be unique");
    }
    Ok(values)
}

fn validate_digest(value: &str) -> Result<(), &'static str> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("transaction fingerprint must be a SHA-256 hex digest");
    }
    Ok(())
}

fn fingerprint<T: Serialize>(value: &T) -> String {
    hex::encode(Sha256::digest(
        serde_json::to_vec(value).expect("commit barrier state serializes"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(value: &[u8]) -> String {
        hex::encode(Sha256::digest(value))
    }

    fn prepared(task: &str, worker: &str, epoch: u64) -> ParticipantVote {
        ParticipantVote {
            worker_id: worker.into(),
            task_id: task.into(),
            lease_epoch: epoch,
            transaction_fingerprint: digest(task.as_bytes()),
            vote: Vote::Prepared,
        }
    }

    #[test]
    fn all_prepared_votes_commit_atomically() {
        let mut barrier = CommitBarrier::create("b", vec!["b".into(), "a".into()], 100).unwrap();
        barrier.record_vote(prepared("a", "w1", 1)).unwrap();
        assert_eq!(barrier.decision, BarrierDecision::Pending);
        barrier.record_vote(prepared("b", "w2", 1)).unwrap();
        assert_eq!(barrier.decision, BarrierDecision::Commit);
        assert_eq!(barrier.prepared_transactions().unwrap().len(), 2);
    }

    #[test]
    fn one_abort_vote_aborts_every_participant() {
        let mut barrier = CommitBarrier::create("b", vec!["a".into(), "b".into()], 100).unwrap();
        barrier.record_vote(prepared("a", "w1", 1)).unwrap();
        let mut vote = prepared("b", "w2", 1);
        vote.vote = Vote::Abort("verification failed".into());
        barrier.record_vote(vote).unwrap();
        assert_eq!(barrier.decision, BarrierDecision::Abort);
        assert_eq!(barrier.rollback_transactions().unwrap().len(), 2);
    }

    #[test]
    fn timeout_and_stale_lease_force_abort() {
        let mut timed = CommitBarrier::create("timed", vec!["a".into()], 10).unwrap();
        timed.expire(10).unwrap();
        assert_eq!(timed.decision, BarrierDecision::Abort);

        let mut stale = CommitBarrier::create("stale", vec!["a".into()], 100).unwrap();
        stale.record_vote(prepared("a", "w1", 1)).unwrap();
        stale.decision = BarrierDecision::Pending;
        stale.refresh_fingerprint();
        stale.validate_active_leases(&BTreeMap::from([("a".into(), 2)])).unwrap();
        assert_eq!(stale.decision, BarrierDecision::Abort);
    }

    #[test]
    fn tampering_is_detected() {
        let mut barrier = CommitBarrier::create("b", vec!["a".into()], 100).unwrap();
        barrier.deadline_ms = 200;
        assert!(barrier.validate().is_err());
    }
}
