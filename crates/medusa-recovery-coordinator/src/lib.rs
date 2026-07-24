use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransactionPhase {
    Created,
    Preparing,
    Prepared,
    Committing,
    Committed,
    Aborting,
    Aborted,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryCandidate {
    pub transaction_id: String,
    pub execution_id: String,
    pub phase: TransactionPhase,
    pub checkpoint_sequence: u64,
    pub checkpoint_fingerprint: String,
    pub snapshot_fingerprint: String,
    pub replay_fingerprint: String,
    pub rollback_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryLock {
    pub transaction_id: String,
    pub owner_id: String,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryAction {
    ResumeCommit,
    ResumeAbort,
    RollBack,
    NoOp,
    Quarantine,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryDecision {
    pub transaction_id: String,
    pub execution_id: String,
    pub action: RecoveryAction,
    pub checkpoint_sequence: u64,
    pub reason: String,
    pub evidence_fingerprint: String,
}

#[derive(Debug, Default)]
pub struct RecoveryCoordinator {
    locks: BTreeMap<String, RecoveryLock>,
    completed: BTreeMap<String, RecoveryDecision>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RecoveryError {
    #[error("invalid sha-256 fingerprint: {0}")]
    InvalidFingerprint(String),
    #[error("duplicate transaction candidate: {0}")]
    DuplicateTransaction(String),
    #[error("recovery lock already held for transaction: {0}")]
    LockHeld(String),
    #[error("stale recovery lock epoch for transaction: {0}")]
    StaleLock(String),
    #[error("recovery lock owner mismatch for transaction: {0}")]
    LockOwnerMismatch(String),
    #[error("recovery decision fingerprint mismatch")]
    DecisionFingerprintMismatch,
}

impl RecoveryCoordinator {
    pub fn discover(
        candidates: impl IntoIterator<Item = RecoveryCandidate>,
    ) -> Result<Vec<RecoveryCandidate>, RecoveryError> {
        let mut by_id = BTreeMap::new();
        for candidate in candidates {
            validate_candidate(&candidate)?;
            if by_id
                .insert(candidate.transaction_id.clone(), candidate)
                .is_some()
            {
                let duplicate = by_id.keys().next_back().cloned().unwrap_or_default();
                return Err(RecoveryError::DuplicateTransaction(duplicate));
            }
        }
        Ok(by_id.into_values().collect())
    }

    pub fn acquire_lock(
        &mut self,
        transaction_id: impl Into<String>,
        owner_id: impl Into<String>,
        epoch: u64,
    ) -> Result<RecoveryLock, RecoveryError> {
        let transaction_id = transaction_id.into();
        let owner_id = owner_id.into();
        if let Some(existing) = self.locks.get(&transaction_id) {
            if epoch <= existing.epoch {
                return Err(RecoveryError::StaleLock(transaction_id));
            }
            return Err(RecoveryError::LockHeld(transaction_id));
        }
        let lock = RecoveryLock {
            transaction_id: transaction_id.clone(),
            owner_id,
            epoch,
        };
        self.locks.insert(transaction_id, lock.clone());
        Ok(lock)
    }

    pub fn release_lock(&mut self, lock: &RecoveryLock) -> Result<(), RecoveryError> {
        match self.locks.get(&lock.transaction_id) {
            Some(current) if current == lock => {
                self.locks.remove(&lock.transaction_id);
                Ok(())
            }
            Some(_) => Err(RecoveryError::LockOwnerMismatch(lock.transaction_id.clone())),
            None => Err(RecoveryError::StaleLock(lock.transaction_id.clone())),
        }
    }

    pub fn decide(
        &mut self,
        candidate: &RecoveryCandidate,
        lock: &RecoveryLock,
    ) -> Result<RecoveryDecision, RecoveryError> {
        validate_candidate(candidate)?;
        self.validate_lock(lock, &candidate.transaction_id)?;

        if let Some(existing) = self.completed.get(&candidate.transaction_id) {
            return Ok(existing.clone());
        }

        let (action, reason) = match candidate.phase {
            TransactionPhase::Committed | TransactionPhase::Aborted => {
                (RecoveryAction::NoOp, "transaction already terminal")
            }
            TransactionPhase::Committing => {
                (RecoveryAction::ResumeCommit, "commit decision is durable")
            }
            TransactionPhase::Aborting => {
                (RecoveryAction::ResumeAbort, "abort decision is durable")
            }
            TransactionPhase::Prepared => {
                (RecoveryAction::RollBack, "prepared transaction lacks durable commit decision")
            }
            TransactionPhase::Created | TransactionPhase::Preparing => {
                (RecoveryAction::RollBack, "transaction did not reach a durable decision")
            }
            TransactionPhase::Failed => {
                (RecoveryAction::Quarantine, "failed transaction requires operator evidence")
            }
        };

        if matches!(action, RecoveryAction::RollBack | RecoveryAction::ResumeAbort)
            && candidate.rollback_fingerprint.is_none()
        {
            let decision = build_decision(candidate, RecoveryAction::Quarantine, "rollback evidence missing");
            self.completed
                .insert(candidate.transaction_id.clone(), decision.clone());
            return Ok(decision);
        }

        let decision = build_decision(candidate, action, reason);
        self.completed
            .insert(candidate.transaction_id.clone(), decision.clone());
        Ok(decision)
    }

    pub fn verify_decision(decision: &RecoveryDecision) -> Result<(), RecoveryError> {
        let expected = decision_fingerprint(
            &decision.transaction_id,
            &decision.execution_id,
            &decision.action,
            decision.checkpoint_sequence,
            &decision.reason,
        );
        if expected == decision.evidence_fingerprint {
            Ok(())
        } else {
            Err(RecoveryError::DecisionFingerprintMismatch)
        }
    }

    fn validate_lock(&self, lock: &RecoveryLock, transaction_id: &str) -> Result<(), RecoveryError> {
        match self.locks.get(transaction_id) {
            Some(current) if current == lock => Ok(()),
            Some(_) => Err(RecoveryError::LockOwnerMismatch(transaction_id.to_owned())),
            None => Err(RecoveryError::StaleLock(transaction_id.to_owned())),
        }
    }
}

fn validate_candidate(candidate: &RecoveryCandidate) -> Result<(), RecoveryError> {
    let mut fingerprints = BTreeSet::new();
    fingerprints.insert(candidate.checkpoint_fingerprint.as_str());
    fingerprints.insert(candidate.snapshot_fingerprint.as_str());
    fingerprints.insert(candidate.replay_fingerprint.as_str());
    if let Some(rollback) = candidate.rollback_fingerprint.as_deref() {
        fingerprints.insert(rollback);
    }
    for fingerprint in fingerprints {
        if fingerprint.len() != 64 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(RecoveryError::InvalidFingerprint(fingerprint.to_owned()));
        }
    }
    Ok(())
}

fn build_decision(
    candidate: &RecoveryCandidate,
    action: RecoveryAction,
    reason: &str,
) -> RecoveryDecision {
    RecoveryDecision {
        transaction_id: candidate.transaction_id.clone(),
        execution_id: candidate.execution_id.clone(),
        checkpoint_sequence: candidate.checkpoint_sequence,
        evidence_fingerprint: decision_fingerprint(
            &candidate.transaction_id,
            &candidate.execution_id,
            &action,
            candidate.checkpoint_sequence,
            reason,
        ),
        action,
        reason: reason.to_owned(),
    }
}

fn decision_fingerprint(
    transaction_id: &str,
    execution_id: &str,
    action: &RecoveryAction,
    checkpoint_sequence: u64,
    reason: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(transaction_id.as_bytes());
    hasher.update([0]);
    hasher.update(execution_id.as_bytes());
    hasher.update([0]);
    hasher.update(format!("{action:?}").as_bytes());
    hasher.update(checkpoint_sequence.to_be_bytes());
    hasher.update(reason.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(value: u8) -> String {
        format!("{value:02x}").repeat(32)
    }

    fn candidate(id: &str, phase: TransactionPhase) -> RecoveryCandidate {
        RecoveryCandidate {
            transaction_id: id.into(),
            execution_id: "exec-1".into(),
            phase,
            checkpoint_sequence: 7,
            checkpoint_fingerprint: fp(1),
            snapshot_fingerprint: fp(2),
            replay_fingerprint: fp(3),
            rollback_fingerprint: Some(fp(4)),
        }
    }

    #[test]
    fn discovery_order_is_deterministic() {
        let discovered = RecoveryCoordinator::discover([
            candidate("tx-b", TransactionPhase::Preparing),
            candidate("tx-a", TransactionPhase::Committing),
        ])
        .unwrap();
        assert_eq!(discovered[0].transaction_id, "tx-a");
        assert_eq!(discovered[1].transaction_id, "tx-b");
    }

    #[test]
    fn committing_transactions_resume_commit() {
        let mut coordinator = RecoveryCoordinator::default();
        let lock = coordinator.acquire_lock("tx", "node-a", 1).unwrap();
        let decision = coordinator
            .decide(&candidate("tx", TransactionPhase::Committing), &lock)
            .unwrap();
        assert_eq!(decision.action, RecoveryAction::ResumeCommit);
        RecoveryCoordinator::verify_decision(&decision).unwrap();
    }

    #[test]
    fn prepared_transactions_roll_back() {
        let mut coordinator = RecoveryCoordinator::default();
        let lock = coordinator.acquire_lock("tx", "node-a", 1).unwrap();
        let decision = coordinator
            .decide(&candidate("tx", TransactionPhase::Prepared), &lock)
            .unwrap();
        assert_eq!(decision.action, RecoveryAction::RollBack);
    }

    #[test]
    fn missing_rollback_evidence_quarantines() {
        let mut coordinator = RecoveryCoordinator::default();
        let lock = coordinator.acquire_lock("tx", "node-a", 1).unwrap();
        let mut item = candidate("tx", TransactionPhase::Prepared);
        item.rollback_fingerprint = None;
        let decision = coordinator.decide(&item, &lock).unwrap();
        assert_eq!(decision.action, RecoveryAction::Quarantine);
    }

    #[test]
    fn recovery_is_idempotent() {
        let mut coordinator = RecoveryCoordinator::default();
        let lock = coordinator.acquire_lock("tx", "node-a", 1).unwrap();
        let item = candidate("tx", TransactionPhase::Committing);
        let first = coordinator.decide(&item, &lock).unwrap();
        let second = coordinator.decide(&item, &lock).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn stale_or_wrong_locks_are_rejected() {
        let mut coordinator = RecoveryCoordinator::default();
        let lock = coordinator.acquire_lock("tx", "node-a", 2).unwrap();
        assert_eq!(
            coordinator.acquire_lock("tx", "node-b", 1),
            Err(RecoveryError::StaleLock("tx".into()))
        );
        let wrong = RecoveryLock { owner_id: "node-b".into(), ..lock };
        assert_eq!(
            coordinator.decide(&candidate("tx", TransactionPhase::Committing), &wrong),
            Err(RecoveryError::LockOwnerMismatch("tx".into()))
        );
    }

    #[test]
    fn tampered_decision_is_rejected() {
        let mut coordinator = RecoveryCoordinator::default();
        let lock = coordinator.acquire_lock("tx", "node-a", 1).unwrap();
        let mut decision = coordinator
            .decide(&candidate("tx", TransactionPhase::Committing), &lock)
            .unwrap();
        decision.reason.push_str(" altered");
        assert_eq!(
            RecoveryCoordinator::verify_decision(&decision),
            Err(RecoveryError::DecisionFingerprintMismatch)
        );
    }
}
