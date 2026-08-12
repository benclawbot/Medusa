use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryHealth {
    Ready,
    NeedsConfirmation,
    Blocked,
    Corrupt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationState {
    Verified,
    Failed,
    Incomplete,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryOperation {
    Inspect,
    Resume,
    RestoreCheckpoint,
    RetryVerification,
    Abandon,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointPresentation {
    pub id: String,
    pub sequence: u64,
    pub created_at_unix_ms: i64,
    pub task_step: String,
    pub reason: String,
    pub repository_fingerprint: String,
    pub verification: VerificationState,
    pub provenance: String,
    pub integrity_verified: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryFileChange {
    pub path: String,
    pub kind: FileChangeKind,
    pub would_overwrite_uncommitted_work: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryPreview {
    pub checkpoint_id: String,
    pub files: Vec<RecoveryFileChange>,
    pub unresolved_risks: Vec<String>,
    pub repository_matches_checkpoint_base: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryActionAvailability {
    pub operation: RecoveryOperation,
    pub enabled: bool,
    pub requires_confirmation: bool,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryView {
    pub session_id: String,
    pub health: RecoveryHealth,
    pub last_durable_step: String,
    pub interrupted_operation: Option<String>,
    pub current_repository_fingerprint: String,
    pub verification: VerificationState,
    pub approvals_must_be_reestablished: bool,
    pub containment_must_be_reestablished: bool,
    pub checkpoints: Vec<CheckpointPresentation>,
    pub selected_preview: Option<RecoveryPreview>,
    pub actions: Vec<RecoveryActionAvailability>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryViewInput {
    pub session_id: String,
    pub last_durable_step: String,
    pub interrupted_operation: Option<String>,
    pub current_repository_fingerprint: String,
    pub verification: VerificationState,
    pub approvals_must_be_reestablished: bool,
    pub containment_must_be_reestablished: bool,
    pub checkpoints: Vec<CheckpointPresentation>,
    pub selected_preview: Option<RecoveryPreview>,
    pub source_corrupt: bool,
}

impl RecoveryView {
    pub fn build(mut input: RecoveryViewInput) -> Self {
        input
            .checkpoints
            .sort_by_key(|checkpoint| checkpoint.sequence);
        input
            .checkpoints
            .dedup_by(|left, right| left.id == right.id);

        let mut warnings = Vec::new();
        if input.source_corrupt {
            warnings.push(
                "Recovery records are corrupt or incomplete; destructive actions are disabled."
                    .to_owned(),
            );
        }
        if input.approvals_must_be_reestablished {
            warnings.push("Previous approvals are not reused after recovery.".to_owned());
        }
        if input.containment_must_be_reestablished {
            warnings
                .push("Containment must pass preflight again before execution resumes.".to_owned());
        }
        if input
            .checkpoints
            .iter()
            .any(|checkpoint| !checkpoint.integrity_verified)
        {
            warnings.push("One or more checkpoints failed integrity verification.".to_owned());
        }

        let preview = input.selected_preview.as_ref();
        let selected_checkpoint_valid = preview.is_some_and(|value| {
            input.checkpoints.iter().any(|checkpoint| {
                checkpoint.id == value.checkpoint_id && checkpoint.integrity_verified
            })
        });
        let stale_or_untrusted_preview = preview.is_some() && !selected_checkpoint_valid;
        if stale_or_untrusted_preview {
            warnings.push(
                "The selected recovery preview does not reference an integrity-verified checkpoint; regenerate the preview."
                    .to_owned(),
            );
        }
        let destructive_conflict = preview.is_some_and(|value| {
            !value.repository_matches_checkpoint_base
                || value
                    .files
                    .iter()
                    .any(|file| file.would_overwrite_uncommitted_work)
                || !value.unresolved_risks.is_empty()
        });
        let valid_checkpoint = input
            .checkpoints
            .iter()
            .any(|checkpoint| checkpoint.integrity_verified);
        let records_blocked = input.source_corrupt || !valid_checkpoint;
        let restore_blocked = records_blocked || stale_or_untrusted_preview;
        let health = if input.source_corrupt {
            RecoveryHealth::Corrupt
        } else if records_blocked || stale_or_untrusted_preview {
            RecoveryHealth::Blocked
        } else if destructive_conflict {
            RecoveryHealth::NeedsConfirmation
        } else {
            RecoveryHealth::Ready
        };

        let actions = vec![
            availability(
                RecoveryOperation::Inspect,
                true,
                false,
                "Inspection never modifies repository state.",
            ),
            availability(
                RecoveryOperation::Resume,
                !records_blocked && !input.containment_must_be_reestablished,
                false,
                if input.containment_must_be_reestablished {
                    "Containment preflight must succeed before resume."
                } else if records_blocked {
                    "Recovery records are not trustworthy enough to resume."
                } else {
                    "Resume from the last durable continuation point."
                },
            ),
            availability(
                RecoveryOperation::RestoreCheckpoint,
                !restore_blocked && preview.is_some(),
                destructive_conflict,
                if preview.is_none() {
                    "Select a checkpoint and generate a preview first."
                } else if records_blocked {
                    "Checkpoint integrity is not sufficient for restore."
                } else if stale_or_untrusted_preview {
                    "The selected preview is stale or references an untrusted checkpoint."
                } else if destructive_conflict {
                    "Restore may overwrite local work or has unresolved risks."
                } else {
                    "Preview is clean and repository preconditions match."
                },
            ),
            availability(
                RecoveryOperation::RetryVerification,
                !records_blocked
                    && matches!(
                        input.verification,
                        VerificationState::Failed | VerificationState::Incomplete
                    ),
                false,
                "Retry verification without claiming completion until required checks pass.",
            ),
            availability(
                RecoveryOperation::Abandon,
                true,
                false,
                "Preserve audit evidence and stop recovery without changing the repository.",
            ),
        ];

        Self {
            session_id: input.session_id,
            health,
            last_durable_step: input.last_durable_step,
            interrupted_operation: input.interrupted_operation,
            current_repository_fingerprint: input.current_repository_fingerprint,
            verification: input.verification,
            approvals_must_be_reestablished: input.approvals_must_be_reestablished,
            containment_must_be_reestablished: input.containment_must_be_reestablished,
            checkpoints: input.checkpoints,
            selected_preview: input.selected_preview,
            actions,
            warnings: deduplicate(warnings),
        }
    }

    pub fn action(&self, operation: RecoveryOperation) -> Option<&RecoveryActionAvailability> {
        self.actions
            .iter()
            .find(|action| action.operation == operation)
    }
}

fn availability(
    operation: RecoveryOperation,
    enabled: bool,
    requires_confirmation: bool,
    reason: &str,
) -> RecoveryActionAvailability {
    RecoveryActionAvailability {
        operation,
        enabled,
        requires_confirmation,
        reason: reason.to_owned(),
    }
}

fn deduplicate(values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkpoint(id: &str, sequence: u64, valid: bool) -> CheckpointPresentation {
        CheckpointPresentation {
            id: id.to_owned(),
            sequence,
            created_at_unix_ms: 1_700_000_000_000,
            task_step: "implement".to_owned(),
            reason: "durable progress".to_owned(),
            repository_fingerprint: "a".repeat(64),
            verification: VerificationState::Incomplete,
            provenance: "execution-checkpoint/v1".to_owned(),
            integrity_verified: valid,
        }
    }

    fn input() -> RecoveryViewInput {
        RecoveryViewInput {
            session_id: "session-1".to_owned(),
            last_durable_step: "implement".to_owned(),
            interrupted_operation: Some("cargo test".to_owned()),
            current_repository_fingerprint: "b".repeat(64),
            verification: VerificationState::Incomplete,
            approvals_must_be_reestablished: false,
            containment_must_be_reestablished: false,
            checkpoints: vec![checkpoint("cp-2", 2, true), checkpoint("cp-1", 1, true)],
            selected_preview: None,
            source_corrupt: false,
        }
    }

    #[test]
    fn sorts_checkpoints_and_requires_preview_before_restore() {
        let view = RecoveryView::build(input());
        assert_eq!(view.checkpoints[0].id, "cp-1");
        assert!(
            !view
                .action(RecoveryOperation::RestoreCheckpoint)
                .unwrap()
                .enabled
        );
        assert_eq!(view.health, RecoveryHealth::Ready);
    }

    #[test]
    fn conflicting_local_work_requires_confirmation() {
        let mut value = input();
        value.selected_preview = Some(RecoveryPreview {
            checkpoint_id: "cp-1".to_owned(),
            files: vec![RecoveryFileChange {
                path: "src/lib.rs".to_owned(),
                kind: FileChangeKind::Modified,
                would_overwrite_uncommitted_work: true,
            }],
            unresolved_risks: Vec::new(),
            repository_matches_checkpoint_base: true,
        });
        let view = RecoveryView::build(value);
        let restore = view.action(RecoveryOperation::RestoreCheckpoint).unwrap();
        assert!(restore.enabled);
        assert!(restore.requires_confirmation);
        assert_eq!(view.health, RecoveryHealth::NeedsConfirmation);
    }

    #[test]
    fn corrupt_records_fail_closed_but_remain_inspectable() {
        let mut value = input();
        value.source_corrupt = true;
        let view = RecoveryView::build(value);
        assert_eq!(view.health, RecoveryHealth::Corrupt);
        assert!(view.action(RecoveryOperation::Inspect).unwrap().enabled);
        assert!(!view.action(RecoveryOperation::Resume).unwrap().enabled);
        assert!(
            !view
                .action(RecoveryOperation::RestoreCheckpoint)
                .unwrap()
                .enabled
        );
        assert!(view.action(RecoveryOperation::Abandon).unwrap().enabled);
    }

    #[test]
    fn missing_checkpoint_preview_fails_closed_without_blocking_resume() {
        let mut value = input();
        value.selected_preview = Some(RecoveryPreview {
            checkpoint_id: "missing".to_owned(),
            files: Vec::new(),
            unresolved_risks: Vec::new(),
            repository_matches_checkpoint_base: true,
        });
        let view = RecoveryView::build(value);
        assert_eq!(view.health, RecoveryHealth::Blocked);
        assert!(view.action(RecoveryOperation::Resume).unwrap().enabled);
        let restore = view.action(RecoveryOperation::RestoreCheckpoint).unwrap();
        assert!(!restore.enabled);
        assert!(restore.reason.contains("stale") || restore.reason.contains("untrusted"));
        assert!(
            view.warnings
                .iter()
                .any(|warning| warning.contains("regenerate"))
        );
    }

    #[test]
    fn integrity_failed_checkpoint_preview_fails_closed() {
        let mut value = input();
        value.checkpoints = vec![
            checkpoint("cp-bad", 1, false),
            checkpoint("cp-good", 2, true),
        ];
        value.selected_preview = Some(RecoveryPreview {
            checkpoint_id: "cp-bad".to_owned(),
            files: Vec::new(),
            unresolved_risks: Vec::new(),
            repository_matches_checkpoint_base: true,
        });
        let view = RecoveryView::build(value);
        assert_eq!(view.health, RecoveryHealth::Blocked);
        assert!(view.action(RecoveryOperation::Resume).unwrap().enabled);
        assert!(
            !view
                .action(RecoveryOperation::RestoreCheckpoint)
                .unwrap()
                .enabled
        );
    }

    #[test]
    fn containment_recheck_blocks_resume() {
        let mut value = input();
        value.containment_must_be_reestablished = true;
        let view = RecoveryView::build(value);
        assert!(!view.action(RecoveryOperation::Resume).unwrap().enabled);
        assert!(
            view.warnings
                .iter()
                .any(|warning| warning.contains("Containment"))
        );
    }
}
