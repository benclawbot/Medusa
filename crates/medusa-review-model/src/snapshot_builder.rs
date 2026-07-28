use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    ChangeKind, ChangeOrigin, ReviewFile, ReviewHunk, ReviewProvenance, ReviewSnapshot,
    ReviewState, VerificationState,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HunkEvidence {
    pub id: String,
    pub base_content: Vec<u8>,
    pub current_content: Vec<u8>,
    pub ambiguous: bool,
    pub overlaps_later_edits: bool,
    pub provenance: ReviewProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeEvidence {
    pub path: String,
    pub previous_path: Option<String>,
    pub kind: ChangeKind,
    pub origin: ChangeOrigin,
    pub binary: bool,
    pub policy_sensitive: bool,
    pub verification: VerificationState,
    pub snapshot_content: Vec<u8>,
    pub current_content: Vec<u8>,
    pub hunks: Vec<HunkEvidence>,
    pub provenance: ReviewProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewSnapshotInput {
    pub repository_fingerprint: String,
    pub created_at_unix_ms: i64,
    pub changes: Vec<ChangeEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
pub enum ReviewSnapshotBuildError {
    #[error("review change path must not be empty")]
    EmptyPath,
    #[error("review snapshot contains duplicate path: {0}")]
    DuplicatePath(String),
    #[error("renamed review change is missing its previous path: {0}")]
    MissingPreviousPath(String),
    #[error("non-renamed review change unexpectedly contains a previous path: {0}")]
    UnexpectedPreviousPath(String),
    #[error("review change contains duplicate hunk id {hunk_id} for {path}")]
    DuplicateHunkId { path: String, hunk_id: String },
    #[error("binary review change must not contain text hunks: {0}")]
    BinaryHasHunks(String),
}

pub fn build_review_snapshot(
    mut input: ReviewSnapshotInput,
) -> Result<ReviewSnapshot, ReviewSnapshotBuildError> {
    input
        .changes
        .sort_by(|left, right| left.path.cmp(&right.path));
    validate_changes(&input.changes)?;

    let files = input
        .changes
        .into_iter()
        .map(build_file)
        .collect::<Vec<_>>();
    let id = snapshot_id(
        &input.repository_fingerprint,
        input.created_at_unix_ms,
        &files,
    );

    Ok(ReviewSnapshot {
        id,
        repository_fingerprint: input.repository_fingerprint,
        created_at_unix_ms: input.created_at_unix_ms,
        files,
    })
}

fn validate_changes(changes: &[ChangeEvidence]) -> Result<(), ReviewSnapshotBuildError> {
    let mut paths = BTreeSet::new();
    for change in changes {
        if change.path.trim().is_empty() {
            return Err(ReviewSnapshotBuildError::EmptyPath);
        }
        if !paths.insert(change.path.clone()) {
            return Err(ReviewSnapshotBuildError::DuplicatePath(change.path.clone()));
        }
        match (change.kind, change.previous_path.as_deref()) {
            (ChangeKind::Renamed, None | Some("")) => {
                return Err(ReviewSnapshotBuildError::MissingPreviousPath(
                    change.path.clone(),
                ));
            }
            (ChangeKind::Renamed, Some(_)) => {}
            (_, Some(_)) => {
                return Err(ReviewSnapshotBuildError::UnexpectedPreviousPath(
                    change.path.clone(),
                ));
            }
            (_, None) => {}
        }
        if change.binary && !change.hunks.is_empty() {
            return Err(ReviewSnapshotBuildError::BinaryHasHunks(
                change.path.clone(),
            ));
        }
        let mut hunk_ids = BTreeSet::new();
        for hunk in &change.hunks {
            if !hunk_ids.insert(hunk.id.clone()) {
                return Err(ReviewSnapshotBuildError::DuplicateHunkId {
                    path: change.path.clone(),
                    hunk_id: hunk.id.clone(),
                });
            }
        }
    }
    Ok(())
}

fn build_file(mut change: ChangeEvidence) -> ReviewFile {
    change.hunks.sort_by(|left, right| left.id.cmp(&right.id));
    ReviewFile {
        path: change.path,
        previous_path: change.previous_path,
        kind: change.kind,
        origin: change.origin,
        binary: change.binary,
        policy_sensitive: change.policy_sensitive,
        verification: change.verification,
        review_state: ReviewState::Unreviewed,
        snapshot_fingerprint: fingerprint(&change.snapshot_content),
        current_fingerprint: fingerprint(&change.current_content),
        hunks: change
            .hunks
            .into_iter()
            .map(|hunk| ReviewHunk {
                id: hunk.id,
                base_fingerprint: fingerprint(&hunk.base_content),
                current_fingerprint: fingerprint(&hunk.current_content),
                ambiguous: hunk.ambiguous,
                overlaps_later_edits: hunk.overlaps_later_edits,
                review_state: ReviewState::Unreviewed,
                provenance: hunk.provenance,
            })
            .collect(),
        provenance: change.provenance,
    }
}

fn snapshot_id(
    repository_fingerprint: &str,
    created_at_unix_ms: i64,
    files: &[ReviewFile],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"medusa-review-snapshot/v1\0");
    hash_field(&mut hasher, repository_fingerprint.as_bytes());
    hash_field(&mut hasher, &created_at_unix_ms.to_le_bytes());
    for file in files {
        hash_field(&mut hasher, file.path.as_bytes());
        hash_field(
            &mut hasher,
            file.previous_path.as_deref().unwrap_or_default().as_bytes(),
        );
        hash_field(&mut hasher, format!("{:?}", file.kind).as_bytes());
        hash_field(&mut hasher, format!("{:?}", file.origin).as_bytes());
        hash_field(&mut hasher, file.snapshot_fingerprint.as_bytes());
        hash_field(&mut hasher, file.current_fingerprint.as_bytes());
        for hunk in &file.hunks {
            hash_field(&mut hasher, hunk.id.as_bytes());
            hash_field(&mut hasher, hunk.base_fingerprint.as_bytes());
            hash_field(&mut hasher, hunk.current_fingerprint.as_bytes());
        }
    }
    format!("review-{}", hex::encode(hasher.finalize()))
}

fn fingerprint(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"medusa-review-content/v1\0");
    hash_field(&mut hasher, content);
    hex::encode(hasher.finalize())
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
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

    fn change(path: &str) -> ChangeEvidence {
        ChangeEvidence {
            path: path.into(),
            previous_path: None,
            kind: ChangeKind::Modified,
            origin: ChangeOrigin::Medusa,
            binary: false,
            policy_sensitive: false,
            verification: VerificationState::Verified,
            snapshot_content: b"before".to_vec(),
            current_content: b"after".to_vec(),
            hunks: vec![HunkEvidence {
                id: "hunk-1".into(),
                base_content: b"before line".to_vec(),
                current_content: b"after line".to_vec(),
                ambiguous: false,
                overlaps_later_edits: false,
                provenance: provenance(),
            }],
            provenance: provenance(),
        }
    }

    #[test]
    fn builds_stable_snapshot_independent_of_input_order() {
        let first = build_review_snapshot(ReviewSnapshotInput {
            repository_fingerprint: "repo-1".into(),
            created_at_unix_ms: 7,
            changes: vec![change("src/z.rs"), change("src/a.rs")],
        })
        .unwrap();
        let second = build_review_snapshot(ReviewSnapshotInput {
            repository_fingerprint: "repo-1".into(),
            created_at_unix_ms: 7,
            changes: vec![change("src/a.rs"), change("src/z.rs")],
        })
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(first.files[0].path, "src/a.rs");
        assert!(first.id.starts_with("review-"));
    }

    #[test]
    fn fingerprints_detect_working_tree_drift() {
        let snapshot = build_review_snapshot(ReviewSnapshotInput {
            repository_fingerprint: "repo-1".into(),
            created_at_unix_ms: 7,
            changes: vec![change("src/lib.rs")],
        })
        .unwrap();
        assert!(snapshot.files[0].has_drift());
        assert_ne!(
            snapshot.files[0].snapshot_fingerprint,
            snapshot.files[0].current_fingerprint
        );
    }

    #[test]
    fn rejects_ambiguous_structural_inputs() {
        let duplicate = build_review_snapshot(ReviewSnapshotInput {
            repository_fingerprint: "repo-1".into(),
            created_at_unix_ms: 7,
            changes: vec![change("src/lib.rs"), change("src/lib.rs")],
        });
        assert_eq!(
            duplicate,
            Err(ReviewSnapshotBuildError::DuplicatePath("src/lib.rs".into()))
        );

        let mut binary = change("asset.bin");
        binary.binary = true;
        let binary_result = build_review_snapshot(ReviewSnapshotInput {
            repository_fingerprint: "repo-1".into(),
            created_at_unix_ms: 7,
            changes: vec![binary],
        });
        assert_eq!(
            binary_result,
            Err(ReviewSnapshotBuildError::BinaryHasHunks("asset.bin".into()))
        );
    }

    #[test]
    fn preserves_origin_verification_and_provenance() {
        let mut generated = change("generated/schema.rs");
        generated.origin = ChangeOrigin::Generated;
        generated.verification = VerificationState::Unverified;
        generated.policy_sensitive = true;
        let snapshot = build_review_snapshot(ReviewSnapshotInput {
            repository_fingerprint: "repo-1".into(),
            created_at_unix_ms: 7,
            changes: vec![generated],
        })
        .unwrap();
        let file = &snapshot.files[0];
        assert_eq!(file.origin, ChangeOrigin::Generated);
        assert_eq!(file.verification, VerificationState::Unverified);
        assert!(file.policy_sensitive);
        assert_eq!(file.provenance.task_step_id.as_deref(), Some("step-1"));
    }
}
