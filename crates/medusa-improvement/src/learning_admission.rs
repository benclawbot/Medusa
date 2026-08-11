//! Shared privacy and admission policy for every learning/refinement surface.

use std::path::Path;

use crate::learning_review::{LearningPrivacy, LearningReviewError, LearningReviewStore};

/// One source of truth for whether a learning-side operation may observe or persist task data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LearningAdmissionPolicy {
    privacy: LearningPrivacy,
}

impl LearningAdmissionPolicy {
    /// Resolve the repository's current privacy revision. Corrupt or unsupported state is an
    /// error so callers can fail closed instead of silently falling back to permissive defaults.
    pub fn for_repository(repo: &Path) -> Result<Self, LearningReviewError> {
        Ok(Self {
            privacy: LearningReviewStore::for_repository(repo).snapshot()?.privacy,
        })
    }

    #[must_use]
    pub const fn from_privacy(privacy: LearningPrivacy) -> Self {
        Self { privacy }
    }

    #[must_use]
    pub const fn privacy(&self) -> &LearningPrivacy {
        &self.privacy
    }

    /// Whether task content may enter any durable learning/refinement side channel.
    #[must_use]
    pub const fn capture_enabled(&self) -> bool {
        self.privacy.capture_enabled
    }

    /// Whether an observed task may automatically produce a candidate/proposal.
    #[must_use]
    pub const fn automatic_proposals_enabled(&self) -> bool {
        self.privacy.capture_enabled && self.privacy.automatic_proposals_enabled
    }

    /// Whether user-level learning storage may be read or written.
    #[must_use]
    pub const fn user_persistence_enabled(&self) -> bool {
        self.privacy.capture_enabled && self.privacy.user_persistence_enabled
    }

    /// Whether a user-level learning may cross repository boundaries.
    #[must_use]
    pub const fn cross_repository_reuse_enabled(&self) -> bool {
        self.user_persistence_enabled() && self.privacy.cross_repository_reuse_enabled
    }

    /// Whether optional learning/tool selection telemetry may be emitted.
    #[must_use]
    pub const fn telemetry_enabled(&self) -> bool {
        self.privacy.capture_enabled && self.privacy.telemetry_enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_is_the_outer_gate_for_every_learning_side_effect() {
        let policy = LearningAdmissionPolicy::from_privacy(LearningPrivacy {
            capture_enabled: false,
            user_persistence_enabled: true,
            cross_repository_reuse_enabled: true,
            telemetry_enabled: true,
            automatic_proposals_enabled: true,
        });
        assert!(!policy.capture_enabled());
        assert!(!policy.automatic_proposals_enabled());
        assert!(!policy.user_persistence_enabled());
        assert!(!policy.cross_repository_reuse_enabled());
        assert!(!policy.telemetry_enabled());
    }

    #[test]
    fn cross_repository_reuse_requires_user_persistence() {
        let policy = LearningAdmissionPolicy::from_privacy(LearningPrivacy {
            capture_enabled: true,
            user_persistence_enabled: false,
            cross_repository_reuse_enabled: true,
            telemetry_enabled: false,
            automatic_proposals_enabled: true,
        });
        assert!(!policy.cross_repository_reuse_enabled());
    }

    #[test]
    fn corrupt_privacy_state_fails_closed() {
        let repo = tempfile::tempdir().expect("repo");
        let root = repo.path().join(".medusa/learning-review");
        std::fs::create_dir_all(&root).expect("root");
        std::fs::write(root.join("state.json"), b"not-json").expect("state");
        assert!(LearningAdmissionPolicy::for_repository(repo.path()).is_err());
    }
}
