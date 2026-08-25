use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ChangeOrigin {
    Medusa,
    PreExistingUser,
    Generated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum VerificationState {
    Verified,
    Failed,
    Stale,
    Unverified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ReviewState {
    Unreviewed,
    Accepted,
    Reverted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewProvenance {
    pub task_step_id: Option<String>,
    pub tool_execution_id: Option<String>,
    pub rationale: Option<String>,
    pub verification_event_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewHunk {
    pub id: String,
    pub base_fingerprint: String,
    pub current_fingerprint: String,
    pub ambiguous: bool,
    pub overlaps_later_edits: bool,
    pub review_state: ReviewState,
    pub provenance: ReviewProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewFile {
    pub path: String,
    pub previous_path: Option<String>,
    pub kind: ChangeKind,
    pub origin: ChangeOrigin,
    pub binary: bool,
    pub policy_sensitive: bool,
    pub verification: VerificationState,
    pub review_state: ReviewState,
    pub snapshot_fingerprint: String,
    pub current_fingerprint: String,
    pub hunks: Vec<ReviewHunk>,
    pub provenance: ReviewProvenance,
}

impl ReviewFile {
    #[must_use]
    pub fn has_drift(&self) -> bool {
        self.snapshot_fingerprint != self.current_fingerprint
    }

    #[must_use]
    pub fn can_accept(&self) -> bool {
        self.origin == ChangeOrigin::Medusa
            && self.review_state != ReviewState::Reverted
            && !matches!(
                self.verification,
                VerificationState::Failed | VerificationState::Stale
            )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewSnapshot {
    pub id: String,
    pub repository_fingerprint: String,
    pub created_at_unix_ms: i64,
    pub files: Vec<ReviewFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionState {
    pub all_required_changes_reviewed: bool,
    pub verification_current: bool,
    pub unreviewed_paths: Vec<String>,
    pub stale_or_failed_paths: Vec<String>,
}

impl CompletionState {
    #[must_use]
    pub fn can_present_verified_completion(&self) -> bool {
        self.all_required_changes_reviewed && self.verification_current
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ReviewFilter {
    pub kinds: BTreeSet<ChangeKind>,
    pub origins: BTreeSet<ChangeOrigin>,
    pub verification_states: BTreeSet<VerificationState>,
    pub policy_sensitive_only: bool,
    pub unreviewed_only: bool,
}

impl ReviewFilter {
    #[must_use]
    pub fn matches(&self, file: &ReviewFile) -> bool {
        (self.kinds.is_empty() || self.kinds.contains(&file.kind))
            && (self.origins.is_empty() || self.origins.contains(&file.origin))
            && (self.verification_states.is_empty()
                || self.verification_states.contains(&file.verification))
            && (!self.policy_sensitive_only || file.policy_sensitive)
            && (!self.unreviewed_only || file.review_state == ReviewState::Unreviewed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewActionRequest {
    AcceptFile {
        path: String,
        expected_snapshot_id: String,
    },
    RevertFile {
        path: String,
        expected_snapshot_id: String,
        expected_file_fingerprint: String,
    },
    RevertHunk {
        path: String,
        hunk_id: String,
        expected_snapshot_id: String,
        expected_file_fingerprint: String,
        expected_hunk_fingerprint: String,
    },
    AcceptTask {
        expected_snapshot_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizedReviewAction {
    pub snapshot_id: String,
    pub action: ReviewActionRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
pub enum ReviewActionRejection {
    #[error("review snapshot is stale; refresh before applying this action")]
    SnapshotMismatch,
    #[error("changed file is not present in the review snapshot")]
    FileNotFound,
    #[error("review snapshot no longer matches the working tree")]
    WorkingTreeDrift,
    #[error("pre-existing user changes cannot be accepted or reverted as Medusa changes")]
    PreExistingUserChange,
    #[error("binary files cannot be selectively reverted")]
    BinaryContent,
    #[error("renamed files require whole-file conflict handling")]
    RenameConflict,
    #[error("requested hunk is not present in the review snapshot")]
    HunkNotFound,
    #[error("requested hunk is ambiguous")]
    AmbiguousHunk,
    #[error("requested hunk overlaps later edits")]
    OverlappingEdits,
    #[error("verification is failed or stale")]
    VerificationNotCurrent,
    #[error("required Medusa changes remain unreviewed")]
    UnreviewedChanges,
}

impl ReviewSnapshot {
    #[must_use]
    pub fn file(&self, path: &str) -> Option<&ReviewFile> {
        self.files.iter().find(|file| file.path == path)
    }

    #[must_use]
    pub fn filtered(&self, filter: &ReviewFilter) -> Vec<&ReviewFile> {
        self.files
            .iter()
            .filter(|file| filter.matches(file))
            .collect()
    }

    #[must_use]
    pub fn change_counts(&self) -> BTreeMap<ChangeKind, usize> {
        let mut counts = BTreeMap::new();
        for file in &self.files {
            *counts.entry(file.kind).or_insert(0) += 1;
        }
        counts
    }

    #[must_use]
    pub fn completion_state(&self) -> CompletionState {
        let required = self
            .files
            .iter()
            .filter(|file| file.origin == ChangeOrigin::Medusa)
            .collect::<Vec<_>>();
        let unreviewed_paths = required
            .iter()
            .filter(|file| file.review_state == ReviewState::Unreviewed)
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        let stale_or_failed_paths = required
            .iter()
            .filter(|file| {
                matches!(
                    file.verification,
                    VerificationState::Failed | VerificationState::Stale
                )
            })
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        CompletionState {
            all_required_changes_reviewed: unreviewed_paths.is_empty(),
            verification_current: stale_or_failed_paths.is_empty(),
            unreviewed_paths,
            stale_or_failed_paths,
        }
    }

    pub fn authorize(
        &self,
        request: ReviewActionRequest,
    ) -> Result<AuthorizedReviewAction, ReviewActionRejection> {
        let expected_snapshot_id = match &request {
            ReviewActionRequest::AcceptFile {
                expected_snapshot_id,
                ..
            }
            | ReviewActionRequest::RevertFile {
                expected_snapshot_id,
                ..
            }
            | ReviewActionRequest::RevertHunk {
                expected_snapshot_id,
                ..
            }
            | ReviewActionRequest::AcceptTask {
                expected_snapshot_id,
            } => expected_snapshot_id,
        };
        if expected_snapshot_id != &self.id {
            return Err(ReviewActionRejection::SnapshotMismatch);
        }

        match &request {
            ReviewActionRequest::AcceptFile { path, .. } => {
                let file = self.file(path).ok_or(ReviewActionRejection::FileNotFound)?;
                reject_non_medusa(file)?;
                if !file.can_accept() {
                    return Err(ReviewActionRejection::VerificationNotCurrent);
                }
            }
            ReviewActionRequest::RevertFile {
                path,
                expected_file_fingerprint,
                ..
            } => authorize_file_revert(self, path, expected_file_fingerprint)?,
            ReviewActionRequest::RevertHunk {
                path,
                hunk_id,
                expected_file_fingerprint,
                expected_hunk_fingerprint,
                ..
            } => authorize_hunk_revert(
                self,
                path,
                hunk_id,
                expected_file_fingerprint,
                expected_hunk_fingerprint,
            )?,
            ReviewActionRequest::AcceptTask { .. } => {
                let completion = self.completion_state();
                if !completion.all_required_changes_reviewed {
                    return Err(ReviewActionRejection::UnreviewedChanges);
                }
                if !completion.verification_current {
                    return Err(ReviewActionRejection::VerificationNotCurrent);
                }
            }
        }

        Ok(AuthorizedReviewAction {
            snapshot_id: self.id.clone(),
            action: request,
        })
    }
}

fn reject_non_medusa(file: &ReviewFile) -> Result<(), ReviewActionRejection> {
    if file.origin == ChangeOrigin::PreExistingUser {
        return Err(ReviewActionRejection::PreExistingUserChange);
    }
    Ok(())
}

fn reject_file_drift(
    file: &ReviewFile,
    expected_file_fingerprint: &str,
) -> Result<(), ReviewActionRejection> {
    if file.has_drift() || file.current_fingerprint != expected_file_fingerprint {
        return Err(ReviewActionRejection::WorkingTreeDrift);
    }
    Ok(())
}

fn authorize_file_revert(
    snapshot: &ReviewSnapshot,
    path: &str,
    expected_file_fingerprint: &str,
) -> Result<(), ReviewActionRejection> {
    let file = snapshot
        .file(path)
        .ok_or(ReviewActionRejection::FileNotFound)?;
    reject_non_medusa(file)?;
    reject_file_drift(file, expected_file_fingerprint)?;
    if file.binary {
        return Err(ReviewActionRejection::BinaryContent);
    }
    if file.kind == ChangeKind::Renamed {
        return Err(ReviewActionRejection::RenameConflict);
    }
    Ok(())
}

fn authorize_hunk_revert(
    snapshot: &ReviewSnapshot,
    path: &str,
    hunk_id: &str,
    expected_file_fingerprint: &str,
    expected_hunk_fingerprint: &str,
) -> Result<(), ReviewActionRejection> {
    authorize_file_revert(snapshot, path, expected_file_fingerprint)?;
    let file = snapshot
        .file(path)
        .ok_or(ReviewActionRejection::FileNotFound)?;
    let hunk = file
        .hunks
        .iter()
        .find(|hunk| hunk.id == hunk_id)
        .ok_or(ReviewActionRejection::HunkNotFound)?;
    if hunk.current_fingerprint != expected_hunk_fingerprint {
        return Err(ReviewActionRejection::WorkingTreeDrift);
    }
    if hunk.ambiguous {
        return Err(ReviewActionRejection::AmbiguousHunk);
    }
    if hunk.overlaps_later_edits {
        return Err(ReviewActionRejection::OverlappingEdits);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provenance() -> ReviewProvenance {
        ReviewProvenance {
            task_step_id: Some("step-1".into()),
            tool_execution_id: Some("tool-1".into()),
            rationale: Some("implement requested change".into()),
            verification_event_ids: vec!["verify-1".into()],
        }
    }

    fn file(path: &str, origin: ChangeOrigin) -> ReviewFile {
        ReviewFile {
            path: path.into(),
            previous_path: None,
            kind: ChangeKind::Modified,
            origin,
            binary: false,
            policy_sensitive: false,
            verification: VerificationState::Verified,
            review_state: ReviewState::Unreviewed,
            snapshot_fingerprint: "file-v1".into(),
            current_fingerprint: "file-v1".into(),
            hunks: vec![ReviewHunk {
                id: "hunk-1".into(),
                base_fingerprint: "hunk-v1".into(),
                current_fingerprint: "hunk-v1".into(),
                ambiguous: false,
                overlaps_later_edits: false,
                review_state: ReviewState::Unreviewed,
                provenance: provenance(),
            }],
            provenance: provenance(),
        }
    }

    fn snapshot(files: Vec<ReviewFile>) -> ReviewSnapshot {
        ReviewSnapshot {
            id: "snapshot-1".into(),
            repository_fingerprint: "repo-v1".into(),
            created_at_unix_ms: 1_700_000_000_000,
            files,
        }
    }

    #[test]
    fn preserves_pre_existing_user_changes() {
        let view = snapshot(vec![file("src/user.rs", ChangeOrigin::PreExistingUser)]);
        let result = view.authorize(ReviewActionRequest::RevertFile {
            path: "src/user.rs".into(),
            expected_snapshot_id: "snapshot-1".into(),
            expected_file_fingerprint: "file-v1".into(),
        });
        assert_eq!(result, Err(ReviewActionRejection::PreExistingUserChange));
    }

    #[test]
    fn working_tree_drift_requires_refresh() {
        let mut changed = file("src/lib.rs", ChangeOrigin::Medusa);
        changed.current_fingerprint = "file-v2".into();
        let view = snapshot(vec![changed]);
        let result = view.authorize(ReviewActionRequest::RevertFile {
            path: "src/lib.rs".into(),
            expected_snapshot_id: "snapshot-1".into(),
            expected_file_fingerprint: "file-v1".into(),
        });
        assert_eq!(result, Err(ReviewActionRejection::WorkingTreeDrift));
    }

    #[test]
    fn ambiguous_hunk_fails_closed() {
        let mut changed = file("src/lib.rs", ChangeOrigin::Medusa);
        changed.hunks[0].ambiguous = true;
        let view = snapshot(vec![changed]);
        let result = view.authorize(ReviewActionRequest::RevertHunk {
            path: "src/lib.rs".into(),
            hunk_id: "hunk-1".into(),
            expected_snapshot_id: "snapshot-1".into(),
            expected_file_fingerprint: "file-v1".into(),
            expected_hunk_fingerprint: "hunk-v1".into(),
        });
        assert_eq!(result, Err(ReviewActionRejection::AmbiguousHunk));
    }

    #[test]
    fn modified_hunk_is_revertible_but_hunk_drift_is_rejected() {
        let mut changed = file("src/lib.rs", ChangeOrigin::Medusa);
        changed.hunks[0].base_fingerprint = "hunk-base".into();
        changed.hunks[0].current_fingerprint = "hunk-current".into();
        let view = snapshot(vec![changed]);
        assert!(
            view.authorize(ReviewActionRequest::RevertHunk {
                path: "src/lib.rs".into(),
                hunk_id: "hunk-1".into(),
                expected_snapshot_id: "snapshot-1".into(),
                expected_file_fingerprint: "file-v1".into(),
                expected_hunk_fingerprint: "hunk-current".into(),
            })
            .is_ok()
        );
        assert_eq!(
            view.authorize(ReviewActionRequest::RevertHunk {
                path: "src/lib.rs".into(),
                hunk_id: "hunk-1".into(),
                expected_snapshot_id: "snapshot-1".into(),
                expected_file_fingerprint: "file-v1".into(),
                expected_hunk_fingerprint: "stale-hunk".into(),
            }),
            Err(ReviewActionRejection::WorkingTreeDrift)
        );
    }

    #[test]
    fn binary_and_renamed_changes_reject_selective_revert() {
        let mut binary = file("asset.bin", ChangeOrigin::Generated);
        binary.binary = true;
        let binary_view = snapshot(vec![binary]);
        assert_eq!(
            binary_view.authorize(ReviewActionRequest::RevertFile {
                path: "asset.bin".into(),
                expected_snapshot_id: "snapshot-1".into(),
                expected_file_fingerprint: "file-v1".into(),
            }),
            Err(ReviewActionRejection::BinaryContent)
        );

        let mut renamed = file("src/new.rs", ChangeOrigin::Medusa);
        renamed.kind = ChangeKind::Renamed;
        renamed.previous_path = Some("src/old.rs".into());
        let renamed_view = snapshot(vec![renamed]);
        assert_eq!(
            renamed_view.authorize(ReviewActionRequest::RevertHunk {
                path: "src/new.rs".into(),
                hunk_id: "hunk-1".into(),
                expected_snapshot_id: "snapshot-1".into(),
                expected_file_fingerprint: "file-v1".into(),
                expected_hunk_fingerprint: "hunk-v1".into(),
            }),
            Err(ReviewActionRejection::RenameConflict)
        );
    }

    #[test]
    fn stale_verification_blocks_verified_completion() {
        let mut changed = file("src/lib.rs", ChangeOrigin::Medusa);
        changed.review_state = ReviewState::Accepted;
        changed.verification = VerificationState::Stale;
        let view = snapshot(vec![changed]);
        let completion = view.completion_state();
        assert!(!completion.can_present_verified_completion());
        assert_eq!(
            view.authorize(ReviewActionRequest::AcceptTask {
                expected_snapshot_id: "snapshot-1".into(),
            }),
            Err(ReviewActionRejection::VerificationNotCurrent)
        );
    }

    #[test]
    fn filters_and_counts_support_client_navigation() {
        let mut generated = file("target/generated.rs", ChangeOrigin::Generated);
        generated.kind = ChangeKind::Added;
        generated.policy_sensitive = true;
        generated.verification = VerificationState::Unverified;
        let view = snapshot(vec![file("src/lib.rs", ChangeOrigin::Medusa), generated]);
        let filter = ReviewFilter {
            origins: BTreeSet::from([ChangeOrigin::Generated]),
            policy_sensitive_only: true,
            ..ReviewFilter::default()
        };
        assert_eq!(view.filtered(&filter).len(), 1);
        assert_eq!(view.change_counts().get(&ChangeKind::Added), Some(&1));
        assert_eq!(view.change_counts().get(&ChangeKind::Modified), Some(&1));
    }
}
