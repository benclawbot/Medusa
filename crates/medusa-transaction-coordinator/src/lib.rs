use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

pub type Fingerprint = String;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransactionPhase {
    Created,
    Resolving,
    Preparing,
    Prepared,
    Committing,
    Committed,
    Aborting,
    Aborted,
    Recovering,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Participant {
    pub worker_id: String,
    pub lease_epoch: u64,
    pub intent_fingerprint: Fingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionRecord {
    pub transaction_id: String,
    pub execution_id: String,
    pub sequence: u64,
    pub phase: TransactionPhase,
    pub participants: Vec<Participant>,
    pub conflict_resolution_fingerprint: Fingerprint,
    pub barrier_fingerprint: Option<Fingerprint>,
    pub rollback_journal_fingerprint: Option<Fingerprint>,
    pub checkpoint_fingerprint: Option<Fingerprint>,
    pub decision_fingerprint: Fingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Vote {
    Prepared { worker_id: String, lease_epoch: u64 },
    Reject { worker_id: String, lease_epoch: u64, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Commit,
    Abort { reason: String },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CoordinatorError {
    #[error("transaction identifier is empty")]
    EmptyTransactionId,
    #[error("execution identifier is empty")]
    EmptyExecutionId,
    #[error("invalid fingerprint")]
    InvalidFingerprint,
    #[error("duplicate participant: {0}")]
    DuplicateParticipant(String),
    #[error("unknown participant: {0}")]
    UnknownParticipant(String),
    #[error("stale lease epoch for {worker_id}: expected {expected}, got {actual}")]
    StaleLease { worker_id: String, expected: u64, actual: u64 },
    #[error("duplicate vote: {0}")]
    DuplicateVote(String),
    #[error("transaction is not in a votable phase")]
    InvalidVotingPhase,
    #[error("transaction transition is invalid")]
    InvalidTransition,
    #[error("transaction fingerprint mismatch")]
    FingerprintMismatch,
}

#[derive(Debug, Default)]
pub struct TransactionCoordinator {
    records: BTreeMap<String, TransactionRecord>,
    votes: BTreeMap<String, BTreeMap<String, Vote>>,
}

impl TransactionCoordinator {
    pub fn create(
        &mut self,
        transaction_id: impl Into<String>,
        execution_id: impl Into<String>,
        sequence: u64,
        mut participants: Vec<Participant>,
        conflict_resolution_fingerprint: Fingerprint,
    ) -> Result<&TransactionRecord, CoordinatorError> {
        let transaction_id = transaction_id.into();
        let execution_id = execution_id.into();
        if transaction_id.trim().is_empty() {
            return Err(CoordinatorError::EmptyTransactionId);
        }
        if execution_id.trim().is_empty() {
            return Err(CoordinatorError::EmptyExecutionId);
        }
        validate_fingerprint(&conflict_resolution_fingerprint)?;
        let mut seen = BTreeSet::new();
        for participant in &participants {
            validate_fingerprint(&participant.intent_fingerprint)?;
            if !seen.insert(participant.worker_id.clone()) {
                return Err(CoordinatorError::DuplicateParticipant(participant.worker_id.clone()));
            }
        }
        participants.sort_by(|a, b| a.worker_id.cmp(&b.worker_id));
        let mut record = TransactionRecord {
            transaction_id: transaction_id.clone(),
            execution_id,
            sequence,
            phase: TransactionPhase::Created,
            participants,
            conflict_resolution_fingerprint,
            barrier_fingerprint: None,
            rollback_journal_fingerprint: None,
            checkpoint_fingerprint: None,
            decision_fingerprint: String::new(),
        };
        record.decision_fingerprint = fingerprint_record(&record);
        self.records.insert(transaction_id.clone(), record);
        self.votes.remove(&transaction_id);
        Ok(self.records.get(&transaction_id).expect("inserted record"))
    }

    pub fn transition(
        &mut self,
        transaction_id: &str,
        next: TransactionPhase,
    ) -> Result<&TransactionRecord, CoordinatorError> {
        let record = self.records.get_mut(transaction_id).ok_or(CoordinatorError::InvalidTransition)?;
        if !valid_transition(&record.phase, &next) {
            return Err(CoordinatorError::InvalidTransition);
        }
        record.phase = next;
        record.decision_fingerprint = fingerprint_record(record);
        Ok(record)
    }

    pub fn attach_evidence(
        &mut self,
        transaction_id: &str,
        barrier: Option<Fingerprint>,
        rollback: Option<Fingerprint>,
        checkpoint: Option<Fingerprint>,
    ) -> Result<&TransactionRecord, CoordinatorError> {
        for value in barrier.iter().chain(rollback.iter()).chain(checkpoint.iter()) {
            validate_fingerprint(value)?;
        }
        let record = self.records.get_mut(transaction_id).ok_or(CoordinatorError::InvalidTransition)?;
        record.barrier_fingerprint = barrier;
        record.rollback_journal_fingerprint = rollback;
        record.checkpoint_fingerprint = checkpoint;
        record.decision_fingerprint = fingerprint_record(record);
        Ok(record)
    }

    pub fn vote(&mut self, transaction_id: &str, vote: Vote) -> Result<(), CoordinatorError> {
        let record = self.records.get(transaction_id).ok_or(CoordinatorError::InvalidVotingPhase)?;
        if !matches!(record.phase, TransactionPhase::Preparing | TransactionPhase::Prepared) {
            return Err(CoordinatorError::InvalidVotingPhase);
        }
        let (worker_id, lease_epoch) = match &vote {
            Vote::Prepared { worker_id, lease_epoch } | Vote::Reject { worker_id, lease_epoch, .. } => (worker_id, *lease_epoch),
        };
        let participant = record.participants.iter().find(|p| &p.worker_id == worker_id)
            .ok_or_else(|| CoordinatorError::UnknownParticipant(worker_id.clone()))?;
        if participant.lease_epoch != lease_epoch {
            return Err(CoordinatorError::StaleLease {
                worker_id: worker_id.clone(),
                expected: participant.lease_epoch,
                actual: lease_epoch,
            });
        }
        let votes = self.votes.entry(transaction_id.to_string()).or_default();
        if votes.insert(worker_id.clone(), vote).is_some() {
            return Err(CoordinatorError::DuplicateVote(worker_id.clone()));
        }
        Ok(())
    }

    pub fn decide(&self, transaction_id: &str) -> Result<Decision, CoordinatorError> {
        let record = self.records.get(transaction_id).ok_or(CoordinatorError::InvalidVotingPhase)?;
        let votes = self.votes.get(transaction_id);
        for participant in &record.participants {
            match votes.and_then(|v| v.get(&participant.worker_id)) {
                Some(Vote::Prepared { .. }) => {}
                Some(Vote::Reject { reason, .. }) => return Ok(Decision::Abort { reason: reason.clone() }),
                None => return Ok(Decision::Abort { reason: format!("missing vote from {}", participant.worker_id) }),
            }
        }
        Ok(Decision::Commit)
    }

    pub fn verify(&self, transaction_id: &str) -> Result<(), CoordinatorError> {
        let record = self.records.get(transaction_id).ok_or(CoordinatorError::FingerprintMismatch)?;
        if record.decision_fingerprint != fingerprint_record(record) {
            return Err(CoordinatorError::FingerprintMismatch);
        }
        Ok(())
    }

    pub fn record(&self, transaction_id: &str) -> Option<&TransactionRecord> {
        self.records.get(transaction_id)
    }
}

fn valid_transition(current: &TransactionPhase, next: &TransactionPhase) -> bool {
    use TransactionPhase::*;
    matches!(
        (current, next),
        (Created, Resolving)
            | (Resolving, Preparing)
            | (Resolving, Aborting)
            | (Preparing, Prepared)
            | (Preparing, Aborting)
            | (Prepared, Committing)
            | (Prepared, Aborting)
            | (Committing, Committed)
            | (Committing, Recovering)
            | (Aborting, Aborted)
            | (Aborting, Recovering)
            | (Recovering, Committing)
            | (Recovering, Aborting)
            | (Recovering, Failed)
    )
}

fn validate_fingerprint(value: &str) -> Result<(), CoordinatorError> {
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(CoordinatorError::InvalidFingerprint);
    }
    Ok(())
}

fn fingerprint_record(record: &TransactionRecord) -> String {
    let mut clone = record.clone();
    clone.decision_fingerprint.clear();
    let encoded = serde_json::to_vec(&clone).expect("serializable transaction record");
    hex::encode(Sha256::digest(encoded))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(byte: u8) -> String { format!("{:02x}", byte).repeat(32) }
    fn participant(id: &str, epoch: u64, byte: u8) -> Participant {
        Participant { worker_id: id.into(), lease_epoch: epoch, intent_fingerprint: fp(byte) }
    }

    #[test]
    fn participant_order_is_deterministic() {
        let mut coordinator = TransactionCoordinator::default();
        let record = coordinator.create("tx", "exec", 1, vec![participant("b", 1, 2), participant("a", 1, 1)], fp(9)).unwrap();
        assert_eq!(record.participants[0].worker_id, "a");
        assert_eq!(record.participants[1].worker_id, "b");
    }

    #[test]
    fn commits_only_after_all_prepared_votes() {
        let mut coordinator = TransactionCoordinator::default();
        coordinator.create("tx", "exec", 1, vec![participant("a", 1, 1), participant("b", 2, 2)], fp(9)).unwrap();
        coordinator.transition("tx", TransactionPhase::Resolving).unwrap();
        coordinator.transition("tx", TransactionPhase::Preparing).unwrap();
        coordinator.vote("tx", Vote::Prepared { worker_id: "a".into(), lease_epoch: 1 }).unwrap();
        assert!(matches!(coordinator.decide("tx").unwrap(), Decision::Abort { .. }));
        coordinator.vote("tx", Vote::Prepared { worker_id: "b".into(), lease_epoch: 2 }).unwrap();
        assert_eq!(coordinator.decide("tx").unwrap(), Decision::Commit);
    }

    #[test]
    fn reject_vote_aborts_deterministically() {
        let mut coordinator = TransactionCoordinator::default();
        coordinator.create("tx", "exec", 1, vec![participant("a", 1, 1)], fp(9)).unwrap();
        coordinator.transition("tx", TransactionPhase::Resolving).unwrap();
        coordinator.transition("tx", TransactionPhase::Preparing).unwrap();
        coordinator.vote("tx", Vote::Reject { worker_id: "a".into(), lease_epoch: 1, reason: "conflict".into() }).unwrap();
        assert_eq!(coordinator.decide("tx").unwrap(), Decision::Abort { reason: "conflict".into() });
    }

    #[test]
    fn stale_lease_is_rejected() {
        let mut coordinator = TransactionCoordinator::default();
        coordinator.create("tx", "exec", 1, vec![participant("a", 3, 1)], fp(9)).unwrap();
        coordinator.transition("tx", TransactionPhase::Resolving).unwrap();
        coordinator.transition("tx", TransactionPhase::Preparing).unwrap();
        assert!(matches!(coordinator.vote("tx", Vote::Prepared { worker_id: "a".into(), lease_epoch: 2 }), Err(CoordinatorError::StaleLease { .. })));
    }

    #[test]
    fn duplicate_votes_are_rejected() {
        let mut coordinator = TransactionCoordinator::default();
        coordinator.create("tx", "exec", 1, vec![participant("a", 1, 1)], fp(9)).unwrap();
        coordinator.transition("tx", TransactionPhase::Resolving).unwrap();
        coordinator.transition("tx", TransactionPhase::Preparing).unwrap();
        coordinator.vote("tx", Vote::Prepared { worker_id: "a".into(), lease_epoch: 1 }).unwrap();
        assert!(matches!(coordinator.vote("tx", Vote::Prepared { worker_id: "a".into(), lease_epoch: 1 }), Err(CoordinatorError::DuplicateVote(_))));
    }

    #[test]
    fn evidence_changes_are_tamper_evident() {
        let mut coordinator = TransactionCoordinator::default();
        coordinator.create("tx", "exec", 1, vec![participant("a", 1, 1)], fp(9)).unwrap();
        coordinator.attach_evidence("tx", Some(fp(4)), Some(fp(5)), Some(fp(6))).unwrap();
        coordinator.verify("tx").unwrap();
        coordinator.records.get_mut("tx").unwrap().checkpoint_fingerprint = Some(fp(7));
        assert_eq!(coordinator.verify("tx"), Err(CoordinatorError::FingerprintMismatch));
    }

    #[test]
    fn invalid_phase_transition_is_rejected() {
        let mut coordinator = TransactionCoordinator::default();
        coordinator.create("tx", "exec", 1, vec![participant("a", 1, 1)], fp(9)).unwrap();
        assert_eq!(coordinator.transition("tx", TransactionPhase::Committed), Err(CoordinatorError::InvalidTransition));
    }
}
