//! Runtime-facing learning lifecycle API shared by TUI and desktop.

use std::path::Path;

pub use medusa_improvement::learning_review::{
    LearningAuditExport, LearningPrivacy, LearningReviewItem, LearningReviewSnapshot,
    LearningReviewState, RedactionPreview,
};
use medusa_improvement::learning_review::{LearningReviewError, LearningReviewStore};

pub fn read(repo: &Path) -> Result<LearningReviewSnapshot, LearningReviewError> {
    LearningReviewStore::for_repository(repo).snapshot()
}

pub fn transition(
    repo: &Path,
    item_id: &str,
    target: LearningReviewState,
    expected_revision: u64,
    actor: &str,
) -> Result<LearningReviewSnapshot, LearningReviewError> {
    LearningReviewStore::for_repository(repo).transition(
        item_id,
        target,
        expected_revision,
        actor,
        now_unix_ms(),
    )
}

pub fn update_privacy(
    repo: &Path,
    privacy: LearningPrivacy,
    expected_revision: u64,
    actor: &str,
) -> Result<LearningReviewSnapshot, LearningReviewError> {
    LearningReviewStore::for_repository(repo).update_privacy(privacy, expected_revision, actor)
}

pub fn redaction_preview(repo: &Path) -> Result<RedactionPreview, LearningReviewError> {
    LearningReviewStore::for_repository(repo).redaction_preview()
}

pub fn export(repo: &Path) -> Result<LearningAuditExport, LearningReviewError> {
    LearningReviewStore::for_repository(repo).export()
}

fn now_unix_ms() -> i64 {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_store_returns_private_empty_snapshot() {
        let repo = tempfile::tempdir().expect("repo");
        let snapshot = read(repo.path()).expect("snapshot");
        assert!(snapshot.items.is_empty());
        assert!(snapshot.privacy.capture_enabled);
        assert!(!snapshot.privacy.cross_repository_reuse_enabled);
        assert!(!snapshot.privacy.telemetry_enabled);
    }
}
