//! Agent-owned transaction safety pipeline.
//!
//! This module is the single composition point for conflict resolution, commit
//! voting, rollback evidence, and the existing atomic repository writer.

use std::{collections::BTreeMap, fs, path::Path};

use medusa_commit_barrier::{BarrierDecision, CommitBarrier, ParticipantVote, Vote as BarrierVote};
use medusa_conflict_resolution::{ConflictResolver, PatchIntent, Resolution};
use medusa_consensus::{ConsensusRecord, Member, Vote as ConsensusVote};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_repository_rollback::{FileState, RecoveryJournal, RollbackEntry};
use medusa_transaction_coordinator::{
    Decision, Participant, TransactionCoordinator, TransactionPhase, Vote as CoordinatorVote,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::transaction::{FileMutation, TransactionOutcome, apply_atomic};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkerMutationProposal {
    pub worker_id: String,
    pub task_id: String,
    pub lease_epoch: u64,
    pub path: String,
    pub expected_fingerprint: String,
    pub content: String,
    pub priority: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransactionEvidence {
    pub transaction_id: String,
    pub barrier_fingerprint: String,
    pub rollback_journal_fingerprint: String,
    pub coordinator_fingerprint: String,
    pub consensus_fingerprint: Option<String>,
    pub affected_files: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SafeTransactionOutcome {
    pub write: TransactionOutcome,
    pub evidence: TransactionEvidence,
}

pub fn execute_safe_transaction(
    repo: &Path,
    transaction_id: &str,
    execution_id: &str,
    sequence: u64,
    deadline_ms: u64,
    proposals: Vec<WorkerMutationProposal>,
) -> MedusaResult<SafeTransactionOutcome> {
    if proposals.is_empty() {
        return Err(validation("transaction requires at least one worker proposal"));
    }

    let required_epochs = proposals
        .iter()
        .map(|proposal| (proposal.worker_id.clone(), proposal.lease_epoch))
        .collect::<BTreeMap<_, _>>();
    let intents = proposals
        .iter()
        .map(|proposal| PatchIntent {
            worker_id: proposal.worker_id.clone(),
            lease_epoch: proposal.lease_epoch,
            path: proposal.path.clone(),
            base_fingerprint: proposal.expected_fingerprint.clone(),
            replacement_fingerprint: fingerprint(proposal.content.as_bytes()),
            priority: proposal.priority,
        })
        .collect::<Vec<_>>();
    let resolutions = ConflictResolver
        .resolve(intents, &required_epochs)
        .map_err(|error| validation(error.to_string()))?;

    let has_competing_proposals = resolutions.iter().any(|resolution| {
        matches!(resolution, Resolution::Identical { workers, .. } if workers.len() > 1)
            || matches!(resolution, Resolution::Conflict(_))
    });
    if resolutions
        .iter()
        .any(|resolution| matches!(resolution, Resolution::Conflict(_)))
    {
        return Err(validation("competing worker proposals require explicit conflict resolution"));
    }

    let consensus_fingerprint = if has_competing_proposals {
        Some(build_consensus(&proposals, sequence)?.fingerprint)
    } else {
        None
    };

    let mutations = select_mutations(&proposals, &resolutions)?;
    verify_read_set(repo, &mutations, &proposals)?;

    let participants = proposals
        .iter()
        .map(|proposal| Participant {
            worker_id: proposal.worker_id.clone(),
            lease_epoch: proposal.lease_epoch,
            intent_fingerprint: fingerprint(
                format!("{}:{}:{}", proposal.task_id, proposal.path, proposal.content).as_bytes(),
            ),
        })
        .collect::<Vec<_>>();
    let resolution_fingerprint = fingerprint(
        &serde_json::to_vec(&resolutions).map_err(|error| validation(error.to_string()))?,
    );
    let mut coordinator = TransactionCoordinator::default();
    coordinator
        .create(
            transaction_id,
            execution_id,
            sequence,
            participants,
            resolution_fingerprint,
        )
        .map_err(|error| validation(error.to_string()))?;
    coordinator
        .transition(transaction_id, TransactionPhase::Resolving)
        .map_err(|error| validation(error.to_string()))?;
    coordinator
        .transition(transaction_id, TransactionPhase::Preparing)
        .map_err(|error| validation(error.to_string()))?;

    let mut barrier = CommitBarrier::create(
        format!("barrier-{transaction_id}"),
        proposals.iter().map(|proposal| proposal.task_id.clone()).collect(),
        deadline_ms,
    )
    .map_err(validation)?;
    for proposal in &proposals {
        let transaction_fingerprint = fingerprint(
            format!("{}:{}:{}", proposal.worker_id, proposal.path, proposal.content).as_bytes(),
        );
        barrier
            .record_vote(ParticipantVote {
                worker_id: proposal.worker_id.clone(),
                task_id: proposal.task_id.clone(),
                lease_epoch: proposal.lease_epoch,
                transaction_fingerprint,
                vote: BarrierVote::Prepared,
            })
            .map_err(validation)?;
        coordinator
            .vote(
                transaction_id,
                CoordinatorVote::Prepared {
                    worker_id: proposal.worker_id.clone(),
                    lease_epoch: proposal.lease_epoch,
                },
            )
            .map_err(|error| validation(error.to_string()))?;
    }
    if barrier.decision != BarrierDecision::Commit
        || coordinator
            .decide(transaction_id)
            .map_err(|error| validation(error.to_string()))?
            != Decision::Commit
    {
        return Err(validation("commit barrier did not authorize mutation"));
    }
    coordinator
        .transition(transaction_id, TransactionPhase::Prepared)
        .map_err(|error| validation(error.to_string()))?;

    let base_snapshot = snapshot_fingerprint(repo, mutations.iter().map(|mutation| mutation.path.as_str()))?;
    let target_snapshot = fingerprint(
        &serde_json::to_vec(&mutations).map_err(|error| validation(error.to_string()))?,
    );
    let rollback_entries = mutations
        .iter()
        .map(|mutation| rollback_entry(repo, mutation))
        .collect::<MedusaResult<Vec<_>>>()?;
    let mut journal = RecoveryJournal::prepare(
        format!("journal-{transaction_id}"),
        base_snapshot,
        target_snapshot,
        barrier.fingerprint.clone(),
        rollback_entries,
    )
    .map_err(validation)?;
    coordinator
        .attach_evidence(
            transaction_id,
            Some(barrier.fingerprint.clone()),
            Some(journal.fingerprint.clone()),
            None,
        )
        .map_err(|error| validation(error.to_string()))?;
    coordinator
        .transition(transaction_id, TransactionPhase::Committing)
        .map_err(|error| validation(error.to_string()))?;

    journal.begin_apply().map_err(validation)?;
    let write = match apply_atomic(repo, &mutations) {
        Ok(write) => write,
        Err(error) => {
            coordinator
                .transition(transaction_id, TransactionPhase::Recovering)
                .map_err(|transition| validation(transition.to_string()))?;
            return Err(error);
        }
    };
    for mutation in &mutations {
        journal.record_applied(&mutation.path).map_err(validation)?;
    }
    journal.finish_apply().map_err(validation)?;
    coordinator
        .attach_evidence(
            transaction_id,
            Some(barrier.fingerprint.clone()),
            Some(journal.fingerprint.clone()),
            Some(snapshot_fingerprint(
                repo,
                mutations.iter().map(|mutation| mutation.path.as_str()),
            )?),
        )
        .map_err(|error| validation(error.to_string()))?;
    coordinator
        .transition(transaction_id, TransactionPhase::Committed)
        .map_err(|error| validation(error.to_string()))?;
    coordinator
        .verify(transaction_id)
        .map_err(|error| validation(error.to_string()))?;
    let record = coordinator
        .record(transaction_id)
        .ok_or_else(|| validation("transaction coordinator lost the committed record"))?;

    Ok(SafeTransactionOutcome {
        write,
        evidence: TransactionEvidence {
            transaction_id: transaction_id.to_owned(),
            barrier_fingerprint: barrier.fingerprint,
            rollback_journal_fingerprint: journal.fingerprint,
            coordinator_fingerprint: record.decision_fingerprint.clone(),
            consensus_fingerprint,
            affected_files: mutations.into_iter().map(|mutation| mutation.path).collect(),
        },
    })
}

fn select_mutations(
    proposals: &[WorkerMutationProposal],
    resolutions: &[Resolution],
) -> MedusaResult<Vec<FileMutation>> {
    let mut selected = Vec::new();
    for resolution in resolutions {
        match resolution {
            Resolution::Apply(intent) => {
                let proposal = proposals
                    .iter()
                    .find(|proposal| {
                        proposal.worker_id == intent.worker_id && proposal.path == intent.path
                    })
                    .ok_or_else(|| validation("resolved proposal is missing"))?;
                selected.push(FileMutation {
                    path: proposal.path.clone(),
                    content: proposal.content.clone(),
                });
            }
            Resolution::Identical { path, workers } => {
                let worker = workers
                    .first()
                    .ok_or_else(|| validation("identical resolution has no workers"))?;
                let proposal = proposals
                    .iter()
                    .find(|proposal| &proposal.worker_id == worker && &proposal.path == path)
                    .ok_or_else(|| validation("identical proposal is missing"))?;
                selected.push(FileMutation {
                    path: proposal.path.clone(),
                    content: proposal.content.clone(),
                });
            }
            Resolution::Conflict(_) => {
                return Err(validation("unresolved conflict reached mutation selection"));
            }
        }
    }
    Ok(selected)
}

fn verify_read_set(
    repo: &Path,
    mutations: &[FileMutation],
    proposals: &[WorkerMutationProposal],
) -> MedusaResult<()> {
    for mutation in mutations {
        let expected = proposals
            .iter()
            .find(|proposal| proposal.path == mutation.path)
            .ok_or_else(|| validation("proposal missing for mutation"))?
            .expected_fingerprint
            .as_str();
        let path = repo.join(&mutation.path);
        let actual = if path.exists() {
            fingerprint(&fs::read(path)?)
        } else {
            fingerprint(&[])
        };
        if actual != expected {
            return Err(validation(format!(
                "stale read set for {}: expected {expected}, found {actual}",
                mutation.path
            )));
        }
    }
    Ok(())
}

fn rollback_entry(repo: &Path, mutation: &FileMutation) -> MedusaResult<RollbackEntry> {
    let path = repo.join(&mutation.path);
    let before = if path.exists() {
        FileState::captured_file(fs::read(&path)?, None)
    } else {
        FileState::Absent
    };
    Ok(RollbackEntry {
        path: mutation.path.clone(),
        before,
        after: FileState::Present {
            content_fingerprint: fingerprint(mutation.content.as_bytes()),
            byte_len: mutation.content.len() as u64,
        },
    })
}

fn snapshot_fingerprint<'a>(
    repo: &Path,
    paths: impl IntoIterator<Item = &'a str>,
) -> MedusaResult<String> {
    let mut state = Vec::new();
    for path in paths {
        let bytes = fs::read(repo.join(path)).unwrap_or_default();
        state.push((path.to_owned(), fingerprint(&bytes)));
    }
    state.sort();
    Ok(fingerprint(
        &serde_json::to_vec(&state).map_err(|error| validation(error.to_string()))?,
    ))
}

fn build_consensus(
    proposals: &[WorkerMutationProposal],
    term: u64,
) -> MedusaResult<ConsensusRecord> {
    let mut workers = proposals
        .iter()
        .map(|proposal| proposal.worker_id.clone())
        .collect::<Vec<_>>();
    workers.sort();
    workers.dedup();
    let candidate = workers
        .first()
        .cloned()
        .ok_or_else(|| validation("consensus requires a worker"))?;
    let mut record = ConsensusRecord::new(
        term.max(1),
        workers
            .iter()
            .map(|id| Member {
                id: id.clone(),
                voting: true,
            })
            .collect(),
    )
    .map_err(|error| validation(error.to_string()))?;
    for voter in workers.iter().take(record.quorum_size()) {
        record
            .cast_vote(ConsensusVote {
                voter: voter.clone(),
                term: term.max(1),
                candidate: candidate.clone(),
            })
            .map_err(|error| validation(error.to_string()))?;
    }
    record
        .advance_commit(&candidate, 1)
        .map_err(|error| validation(error.to_string()))?;
    Ok(record)
}

fn fingerprint(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn validation(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal(worker: &str, task: &str, path: &str, old: &[u8], new: &str) -> WorkerMutationProposal {
        WorkerMutationProposal {
            worker_id: worker.into(),
            task_id: task.into(),
            lease_epoch: 1,
            path: path.into(),
            expected_fingerprint: fingerprint(old),
            content: new.into(),
            priority: 10,
        }
    }

    #[test]
    fn disjoint_workers_commit_as_one_transaction_without_consensus() {
        let repo = tempfile::tempdir().unwrap();
        fs::write(repo.path().join("a.txt"), "old-a").unwrap();
        fs::write(repo.path().join("b.txt"), "old-b").unwrap();
        let result = execute_safe_transaction(
            repo.path(),
            "txn-1",
            "exec-1",
            1,
            100,
            vec![
                proposal("w1", "t1", "a.txt", b"old-a", "new-a"),
                proposal("w2", "t2", "b.txt", b"old-b", "new-b"),
            ],
        )
        .unwrap();
        assert_eq!(fs::read_to_string(repo.path().join("a.txt")).unwrap(), "new-a");
        assert_eq!(fs::read_to_string(repo.path().join("b.txt")).unwrap(), "new-b");
        assert!(result.evidence.consensus_fingerprint.is_none());
    }

    #[test]
    fn stale_read_set_fails_before_any_write() {
        let repo = tempfile::tempdir().unwrap();
        fs::write(repo.path().join("a.txt"), "current").unwrap();
        let result = execute_safe_transaction(
            repo.path(),
            "txn-2",
            "exec-2",
            1,
            100,
            vec![proposal("w1", "t1", "a.txt", b"stale", "new")],
        );
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(repo.path().join("a.txt")).unwrap(), "current");
    }

    #[test]
    fn competing_identical_proposals_use_consensus_once() {
        let repo = tempfile::tempdir().unwrap();
        fs::write(repo.path().join("a.txt"), "old").unwrap();
        let result = execute_safe_transaction(
            repo.path(),
            "txn-3",
            "exec-3",
            2,
            100,
            vec![
                proposal("w1", "t1", "a.txt", b"old", "same"),
                proposal("w2", "t2", "a.txt", b"old", "same"),
            ],
        )
        .unwrap();
        assert!(result.evidence.consensus_fingerprint.is_some());
        assert_eq!(fs::read_to_string(repo.path().join("a.txt")).unwrap(), "same");
    }

    #[test]
    fn conflicting_replacements_fail_closed() {
        let repo = tempfile::tempdir().unwrap();
        fs::write(repo.path().join("a.txt"), "old").unwrap();
        let result = execute_safe_transaction(
            repo.path(),
            "txn-4",
            "exec-4",
            2,
            100,
            vec![
                proposal("w1", "t1", "a.txt", b"old", "one"),
                proposal("w2", "t2", "a.txt", b"old", "two"),
            ],
        );
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(repo.path().join("a.txt")).unwrap(), "old");
    }
}
