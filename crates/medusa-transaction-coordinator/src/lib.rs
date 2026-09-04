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
    Prepared {
        worker_id: String,
        lease_epoch: u64,
    },
    Reject {
        worker_id: String,
        lease_epoch: u64,
        reason: String,
    },
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
    StaleLease {
        worker_id: String,
        expected: u64,
        actual: u64,
    },
    #[error("duplicate vote: {0}")]
    DuplicateVote(String),
    #[error("transaction is not in a votable phase")]
    InvalidVotingPhase,
    #[error("transaction transition is invalid")]
    InvalidTransition,
    #[error("transaction fingerprint mismatch")]
    FingerprintMismatch,
    #[error("failed to serialize transaction state: {0}")]
    Serialization(String),
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
                return Err(CoordinatorError::DuplicateParticipant(
                    participant.worker_id.clone(),
                ));
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
        record.decision_fingerprint = fingerprint_record(&record)?;
        self.records.insert(transaction_id.clone(), record);
        self.votes.remove(&transaction_id);
        self.records
            .get(&transaction_id)
            .ok_or(CoordinatorError::InvalidTransition)
    }

    pub fn transition(
        &mut self,
        transaction_id: &str,
        next: TransactionPhase,
    ) -> Result<&TransactionRecord, CoordinatorError> {
        let record = self
            .records
            .get_mut(transaction_id)
            .ok_or(CoordinatorError::InvalidTransition)?;
        if !valid_transition(&record.phase, &next) {
            return Err(CoordinatorError::InvalidTransition);
        }
        record.phase = next;
        record.decision_fingerprint = fingerprint_record(record)?;
        Ok(record)
    }

    pub fn attach_evidence(
        &mut self,
        transaction_id: &str,
        barrier: Option<Fingerprint>,
        rollback: Option<Fingerprint>,
        checkpoint: Option<Fingerprint>,
    ) -> Result<&TransactionRecord, CoordinatorError> {
        for value in barrier
            .iter()
            .chain(rollback.iter())
            .chain(checkpoint.iter())
        {
            validate_fingerprint(value)?;
        }
        let record = self
            .records
            .get_mut(transaction_id)
            .ok_or(CoordinatorError::InvalidTransition)?;
        record.barrier_fingerprint = barrier;
        record.rollback_journal_fingerprint = rollback;
        record.checkpoint_fingerprint = checkpoint;
        record.decision_fingerprint = fingerprint_record(record)?;
        Ok(record)
    }

    pub fn vote(&mut self, transaction_id: &str, vote: Vote) -> Result<(), CoordinatorError> {
        let record = self
            .records
            .get(transaction_id)
            .ok_or(CoordinatorError::InvalidVotingPhase)?;
        if !matches!(
            record.phase,
            TransactionPhase::Preparing | TransactionPhase::Prepared
        ) {
            return Err(CoordinatorError::InvalidVotingPhase);
        }
        let (worker_id, lease_epoch) = match &vote {
            Vote::Prepared {
                worker_id,
                lease_epoch,
            }
            | Vote::Reject {
                worker_id,
                lease_epoch,
                ..
            } => (worker_id.clone(), *lease_epoch),
        };
        let participant = record
            .participants
            .iter()
            .find(|participant| participant.worker_id == worker_id)
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
            return Err(CoordinatorError::DuplicateVote(worker_id));
        }
        Ok(())
    }

    pub fn decide(&self, transaction_id: &str) -> Result<Decision, CoordinatorError> {
        let record = self
            .records
            .get(transaction_id)
            .ok_or(CoordinatorError::InvalidVotingPhase)?;
        let votes = self.votes.get(transaction_id);
        for participant in &record.participants {
            match votes.and_then(|entries| entries.get(&participant.worker_id)) {
                Some(Vote::Prepared { .. }) => {}
                Some(Vote::Reject { reason, .. }) => {
                    return Ok(Decision::Abort {
                        reason: reason.clone(),
                    });
                }
                None => {
                    return Ok(Decision::Abort {
                        reason: format!("missing vote from {}", participant.worker_id),
                    });
                }
            }
        }
        Ok(Decision::Commit)
    }

    pub fn verify(&self, transaction_id: &str) -> Result<(), CoordinatorError> {
        let record = self
            .records
            .get(transaction_id)
            .ok_or(CoordinatorError::FingerprintMismatch)?;
        if record.decision_fingerprint != fingerprint_record(record)? {
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
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CoordinatorError::InvalidFingerprint);
    }
    Ok(())
}

fn fingerprint_record(record: &TransactionRecord) -> Result<String, CoordinatorError> {
    let mut clone = record.clone();
    clone.decision_fingerprint.clear();
    let encoded = serde_json::to_vec(&clone)
        .map_err(|error| CoordinatorError::Serialization(error.to_string()))?;
    Ok(hex::encode(Sha256::digest(encoded)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(seed: u8) -> Fingerprint {
        hex::encode([seed; 32])
    }

    fn two_participants() -> Vec<Participant> {
        vec![
            Participant {
                worker_id: "b".to_owned(),
                lease_epoch: 1,
                intent_fingerprint: fp(1),
            },
            Participant {
                worker_id: "a".to_owned(),
                lease_epoch: 2,
                intent_fingerprint: fp(2),
            },
        ]
    }

    fn created() -> (TransactionCoordinator, String) {
        let mut coordinator = TransactionCoordinator::default();
        coordinator
            .create("tx-1", "exec-1", 0, two_participants(), fp(9))
            .expect("create");
        (coordinator, "tx-1".to_owned())
    }

    fn drive(coordinator: &mut TransactionCoordinator, id: &str, phases: &[TransactionPhase]) {
        for next in phases {
            coordinator.transition(id, next.clone()).expect("transition");
        }
    }

    #[test]
    fn create_sorts_participants_and_fingerprints() {
        let (coordinator, id) = created();
        let record = coordinator.record(&id).expect("record");
        assert_eq!(record.phase, TransactionPhase::Created);
        assert_eq!(record.participants[0].worker_id, "a");
        coordinator.verify(&id).expect("verify");
    }

    #[test]
    fn create_rejects_bad_input() {
        let mut coordinator = TransactionCoordinator::default();
        assert_eq!(
            coordinator.create("", "exec", 0, two_participants(), fp(9)),
            Err(CoordinatorError::EmptyTransactionId)
        );
        assert_eq!(
            coordinator.create("tx", "", 0, two_participants(), fp(9)),
            Err(CoordinatorError::EmptyExecutionId)
        );
        assert_eq!(
            coordinator.create("tx", "exec", 0, two_participants(), "nope".to_owned()),
            Err(CoordinatorError::InvalidFingerprint)
        );
        let mut dupes = two_participants();
        dupes.push(dupes[0].clone());
        assert!(matches!(
            coordinator.create("tx", "exec", 0, dupes, fp(9)),
            Err(CoordinatorError::DuplicateParticipant(_))
        ));
    }

    #[test]
    fn unanimous_prepared_votes_commit() {
        let (mut coordinator, id) = created();
        drive(
            &mut coordinator,
            &id,
            &[
                TransactionPhase::Resolving,
                TransactionPhase::Preparing,
                TransactionPhase::Prepared,
            ],
        );
        for (worker, epoch) in [("a", 2), ("b", 1)] {
            coordinator
                .vote(
                    &id,
                    Vote::Prepared {
                        worker_id: worker.to_owned(),
                        lease_epoch: epoch,
                    },
                )
                .expect("vote");
        }
        assert_eq!(
            coordinator.decide(&id).expect("decide"),
            Decision::Commit
        );
    }

    #[test]
    fn reject_or_missing_vote_aborts() {
        let (mut coordinator, id) = created();
        drive(
            &mut coordinator,
            &id,
            &[
                TransactionPhase::Resolving,
                TransactionPhase::Preparing,
                TransactionPhase::Prepared,
            ],
        );
        // Missing vote from b aborts first.
        assert!(matches!(
            coordinator.decide(&id).expect("decide"),
            Decision::Abort { .. }
        ));
        coordinator
            .vote(
                &id,
                Vote::Reject {
                    worker_id: "a".to_owned(),
                    lease_epoch: 2,
                    reason: "disk full".to_owned(),
                },
            )
            .expect("vote");
        assert_eq!(
            coordinator.decide(&id).expect("decide"),
            Decision::Abort {
                reason: "disk full".to_owned()
            }
        );
    }

    #[test]
    fn vote_guards_unknown_stale_and_duplicate() {
        let (mut coordinator, id) = created();
        // Voting outside Preparing/Prepared is rejected.
        assert_eq!(
            coordinator.vote(
                &id,
                Vote::Prepared {
                    worker_id: "a".to_owned(),
                    lease_epoch: 2,
                }
            ),
            Err(CoordinatorError::InvalidVotingPhase)
        );
        drive(
            &mut coordinator,
            &id,
            &[
                TransactionPhase::Resolving,
                TransactionPhase::Preparing,
            ],
        );
        assert!(matches!(
            coordinator.vote(
                &id,
                Vote::Prepared {
                    worker_id: "ghost".to_owned(),
                    lease_epoch: 0,
                }
            ),
            Err(CoordinatorError::UnknownParticipant(_))
        ));
        assert!(matches!(
            coordinator.vote(
                &id,
                Vote::Prepared {
                    worker_id: "a".to_owned(),
                    lease_epoch: 99,
                }
            ),
            Err(CoordinatorError::StaleLease { .. })
        ));
        coordinator
            .vote(
                &id,
                Vote::Prepared {
                    worker_id: "a".to_owned(),
                    lease_epoch: 2,
                },
            )
            .expect("vote");
        assert!(matches!(
            coordinator.vote(
                &id,
                Vote::Prepared {
                    worker_id: "a".to_owned(),
                    lease_epoch: 2,
                }
            ),
            Err(CoordinatorError::DuplicateVote(_))
        ));
    }

    #[test]
    fn illegal_transitions_rejected() {
        let (mut coordinator, id) = created();
        assert_eq!(
            coordinator.transition(&id, TransactionPhase::Committed),
            Err(CoordinatorError::InvalidTransition)
        );
        assert_eq!(
            coordinator.transition("missing", TransactionPhase::Resolving),
            Err(CoordinatorError::InvalidTransition)
        );
    }

    #[test]
    fn evidence_and_tamper_detection() {
        let (mut coordinator, id) = created();
        coordinator
            .attach_evidence(&id, Some(fp(3)), None, Some(fp(4)))
            .expect("attach");
        coordinator.verify(&id).expect("verify");
        let record = coordinator
            .records
            .get_mut(&id)
            .expect("record")
            .clone();
        coordinator.records.get_mut(&id).expect("record").sequence = record.sequence + 1;
        assert_eq!(
            coordinator.verify(&id),
            Err(CoordinatorError::FingerprintMismatch)
        );
    }
}
