use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{AuthorizedReviewAction, ReviewActionRequest, ReviewSnapshot, ReviewState};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewAuditScope {
    File { path: String },
    Hunk { path: String, hunk_id: String },
    Task,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewAuditDecision {
    Accepted,
    Reverted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewAuditEvent {
    pub id: String,
    pub snapshot_id: String,
    pub scope: ReviewAuditScope,
    pub decision: ReviewAuditDecision,
    pub actor: String,
    pub occurred_at_unix_ms: i64,
    pub repository_fingerprint_before: String,
    pub repository_fingerprint_after: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
pub enum ReviewAuditError {
    #[error("authorized action does not target the current review snapshot")]
    SnapshotMismatch,
    #[error("review actor must not be empty")]
    EmptyActor,
    #[error("resulting repository fingerprint must not be empty")]
    EmptyRepositoryFingerprint,
    #[error("review action timestamp must not be negative")]
    InvalidTimestamp,
    #[error("changed file is not present in the review snapshot")]
    FileNotFound,
    #[error("requested hunk is not present in the review snapshot")]
    HunkNotFound,
    #[error("review scope has already reached a conflicting terminal state")]
    ConflictingReviewState,
}

pub fn record_authorized_action(
    snapshot: &mut ReviewSnapshot,
    authorized: AuthorizedReviewAction,
    actor: impl Into<String>,
    occurred_at_unix_ms: i64,
    repository_fingerprint_after: impl Into<String>,
) -> Result<ReviewAuditEvent, ReviewAuditError> {
    if authorized.snapshot_id != snapshot.id {
        return Err(ReviewAuditError::SnapshotMismatch);
    }

    let actor = actor.into();
    if actor.trim().is_empty() {
        return Err(ReviewAuditError::EmptyActor);
    }
    if occurred_at_unix_ms < 0 {
        return Err(ReviewAuditError::InvalidTimestamp);
    }
    let repository_fingerprint_after = repository_fingerprint_after.into();
    if repository_fingerprint_after.trim().is_empty() {
        return Err(ReviewAuditError::EmptyRepositoryFingerprint);
    }

    let (scope, decision) = apply_action(snapshot, &authorized.action)?;
    let event_id = event_id(
        &snapshot.id,
        &scope,
        decision,
        &actor,
        occurred_at_unix_ms,
        &snapshot.repository_fingerprint,
        &repository_fingerprint_after,
    );

    Ok(ReviewAuditEvent {
        id: event_id,
        snapshot_id: snapshot.id.clone(),
        scope,
        decision,
        actor,
        occurred_at_unix_ms,
        repository_fingerprint_before: snapshot.repository_fingerprint.clone(),
        repository_fingerprint_after,
    })
}

fn apply_action(
    snapshot: &mut ReviewSnapshot,
    action: &ReviewActionRequest,
) -> Result<(ReviewAuditScope, ReviewAuditDecision), ReviewAuditError> {
    match action {
        ReviewActionRequest::AcceptFile { path, .. } => {
            let file = snapshot
                .files
                .iter_mut()
                .find(|file| file.path == *path)
                .ok_or(ReviewAuditError::FileNotFound)?;
            transition(&mut file.review_state, ReviewState::Accepted)?;
            for hunk in &mut file.hunks {
                transition(&mut hunk.review_state, ReviewState::Accepted)?;
            }
            Ok((
                ReviewAuditScope::File { path: path.clone() },
                ReviewAuditDecision::Accepted,
            ))
        }
        ReviewActionRequest::RevertFile { path, .. } => {
            let file = snapshot
                .files
                .iter_mut()
                .find(|file| file.path == *path)
                .ok_or(ReviewAuditError::FileNotFound)?;
            transition(&mut file.review_state, ReviewState::Reverted)?;
            for hunk in &mut file.hunks {
                transition(&mut hunk.review_state, ReviewState::Reverted)?;
            }
            Ok((
                ReviewAuditScope::File { path: path.clone() },
                ReviewAuditDecision::Reverted,
            ))
        }
        ReviewActionRequest::RevertHunk { path, hunk_id, .. } => {
            let file = snapshot
                .files
                .iter_mut()
                .find(|file| file.path == *path)
                .ok_or(ReviewAuditError::FileNotFound)?;
            let hunk = file
                .hunks
                .iter_mut()
                .find(|hunk| hunk.id == *hunk_id)
                .ok_or(ReviewAuditError::HunkNotFound)?;
            transition(&mut hunk.review_state, ReviewState::Reverted)?;
            if file
                .hunks
                .iter()
                .all(|hunk| hunk.review_state == ReviewState::Reverted)
            {
                transition(&mut file.review_state, ReviewState::Reverted)?;
            }
            Ok((
                ReviewAuditScope::Hunk {
                    path: path.clone(),
                    hunk_id: hunk_id.clone(),
                },
                ReviewAuditDecision::Reverted,
            ))
        }
        ReviewActionRequest::AcceptTask { .. } => {
            Ok((ReviewAuditScope::Task, ReviewAuditDecision::Accepted))
        }
    }
}

fn transition(current: &mut ReviewState, target: ReviewState) -> Result<(), ReviewAuditError> {
    match (*current, target) {
        (ReviewState::Unreviewed, next) => {
            *current = next;
            Ok(())
        }
        (existing, requested) if existing == requested => Ok(()),
        _ => Err(ReviewAuditError::ConflictingReviewState),
    }
}

fn event_id(
    snapshot_id: &str,
    scope: &ReviewAuditScope,
    decision: ReviewAuditDecision,
    actor: &str,
    occurred_at_unix_ms: i64,
    repository_fingerprint_before: &str,
    repository_fingerprint_after: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"medusa-review-audit/v1\0");
    hash_field(&mut hasher, snapshot_id.as_bytes());
    hash_field(&mut hasher, format!("{scope:?}").as_bytes());
    hash_field(&mut hasher, format!("{decision:?}").as_bytes());
    hash_field(&mut hasher, actor.as_bytes());
    hash_field(&mut hasher, &occurred_at_unix_ms.to_le_bytes());
    hash_field(&mut hasher, repository_fingerprint_before.as_bytes());
    hash_field(&mut hasher, repository_fingerprint_after.as_bytes());
    format!("review-event-{}", hex::encode(hasher.finalize()))
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

#[cfg(test)]
mod tests {
    use crate::{
        ChangeKind, ChangeOrigin, ReviewFile, ReviewHunk, ReviewProvenance, VerificationState,
    };

    use super::*;

    fn snapshot() -> ReviewSnapshot {
        let provenance = ReviewProvenance {
            task_step_id: Some("step-1".into()),
            tool_execution_id: Some("tool-1".into()),
            rationale: None,
            verification_event_ids: vec![],
        };
        ReviewSnapshot {
            id: "snapshot-1".into(),
            repository_fingerprint: "repo-before".into(),
            created_at_unix_ms: 1,
            files: vec![ReviewFile {
                path: "src/lib.rs".into(),
                previous_path: None,
                kind: ChangeKind::Modified,
                origin: ChangeOrigin::Medusa,
                binary: false,
                policy_sensitive: false,
                verification: VerificationState::Verified,
                review_state: ReviewState::Unreviewed,
                snapshot_fingerprint: "file-1".into(),
                current_fingerprint: "file-1".into(),
                hunks: vec![ReviewHunk {
                    id: "hunk-1".into(),
                    base_fingerprint: "hunk-1".into(),
                    current_fingerprint: "hunk-1".into(),
                    ambiguous: false,
                    overlaps_later_edits: false,
                    review_state: ReviewState::Unreviewed,
                    provenance: provenance.clone(),
                }],
                provenance,
            }],
        }
    }

    #[test]
    fn records_file_acceptance_and_updates_review_state() {
        let mut snapshot = snapshot();
        let authorized = snapshot
            .authorize(ReviewActionRequest::AcceptFile {
                path: "src/lib.rs".into(),
                expected_snapshot_id: snapshot.id.clone(),
            })
            .unwrap();
        let event =
            record_authorized_action(&mut snapshot, authorized, "user:alice", 42, "repo-after")
                .unwrap();

        assert_eq!(snapshot.files[0].review_state, ReviewState::Accepted);
        assert_eq!(
            snapshot.files[0].hunks[0].review_state,
            ReviewState::Accepted
        );
        assert_eq!(event.decision, ReviewAuditDecision::Accepted);
        assert_eq!(event.repository_fingerprint_before, "repo-before");
        assert_eq!(event.repository_fingerprint_after, "repo-after");
        assert!(event.id.starts_with("review-event-"));
    }

    #[test]
    fn records_hunk_revert_and_promotes_fully_reverted_file() {
        let mut snapshot = snapshot();
        let authorized = snapshot
            .authorize(ReviewActionRequest::RevertHunk {
                path: "src/lib.rs".into(),
                hunk_id: "hunk-1".into(),
                expected_snapshot_id: snapshot.id.clone(),
                expected_file_fingerprint: "file-1".into(),
                expected_hunk_fingerprint: "hunk-1".into(),
            })
            .unwrap();
        let event =
            record_authorized_action(&mut snapshot, authorized, "user:alice", 43, "repo-after")
                .unwrap();

        assert_eq!(snapshot.files[0].review_state, ReviewState::Reverted);
        assert_eq!(event.decision, ReviewAuditDecision::Reverted);
        assert_eq!(
            event.scope,
            ReviewAuditScope::Hunk {
                path: "src/lib.rs".into(),
                hunk_id: "hunk-1".into(),
            }
        );
    }

    #[test]
    fn rejects_conflicting_terminal_transition() {
        let mut snapshot = snapshot();
        snapshot.files[0].review_state = ReviewState::Accepted;
        let authorized = AuthorizedReviewAction {
            snapshot_id: snapshot.id.clone(),
            action: ReviewActionRequest::RevertFile {
                path: "src/lib.rs".into(),
                expected_snapshot_id: snapshot.id.clone(),
                expected_file_fingerprint: "file-1".into(),
            },
        };

        assert_eq!(
            record_authorized_action(&mut snapshot, authorized, "user:alice", 44, "repo-after",),
            Err(ReviewAuditError::ConflictingReviewState)
        );
    }

    #[test]
    fn event_identity_is_deterministic() {
        let mut first = snapshot();
        let mut second = snapshot();
        let action = AuthorizedReviewAction {
            snapshot_id: "snapshot-1".into(),
            action: ReviewActionRequest::AcceptFile {
                path: "src/lib.rs".into(),
                expected_snapshot_id: "snapshot-1".into(),
            },
        };
        let first_event =
            record_authorized_action(&mut first, action.clone(), "user:alice", 45, "repo-after")
                .unwrap();
        let second_event =
            record_authorized_action(&mut second, action, "user:alice", 45, "repo-after").unwrap();
        assert_eq!(first_event.id, second_event.id);
    }
}
