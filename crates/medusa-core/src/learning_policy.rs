//! Privacy admission policy shared by every learning/refinement producer and consumer.

use std::{fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

const LEARNING_REVIEW_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LearningPrivacyPolicy {
    pub capture_enabled: bool,
    pub user_persistence_enabled: bool,
    pub cross_repository_reuse_enabled: bool,
    pub telemetry_enabled: bool,
    pub automatic_proposals_enabled: bool,
}

impl LearningPrivacyPolicy {
    #[must_use]
    pub const fn private_by_default() -> Self {
        Self {
            capture_enabled: true,
            user_persistence_enabled: false,
            cross_repository_reuse_enabled: false,
            telemetry_enabled: false,
            automatic_proposals_enabled: true,
        }
    }
}

#[derive(Deserialize)]
struct PolicyDocument {
    schema_version: u32,
    privacy: LearningPrivacyPolicy,
}

/// One fail-closed policy used before learning-side reads, persistence, proposal generation,
/// user-scope reuse, or optional telemetry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LearningAdmissionPolicy {
    privacy: LearningPrivacyPolicy,
}

impl LearningAdmissionPolicy {
    pub fn for_repository(repo: &Path) -> MedusaResult<Self> {
        let path = repo.join(".medusa/learning-review/state.json");
        if !path.exists() {
            return Ok(Self::from_privacy(
                LearningPrivacyPolicy::private_by_default(),
            ));
        }
        let document: PolicyDocument =
            serde_json::from_slice(&fs::read(&path)?).map_err(|error| {
                policy_error(format!(
                    "learning privacy state {} is invalid; learning failed closed: {error}",
                    path.display()
                ))
            })?;
        if document.schema_version != LEARNING_REVIEW_SCHEMA_VERSION {
            return Err(policy_error(format!(
                "unsupported learning privacy schema {}; learning failed closed",
                document.schema_version
            )));
        }
        Ok(Self::from_privacy(document.privacy))
    }

    #[must_use]
    pub const fn from_privacy(privacy: LearningPrivacyPolicy) -> Self {
        Self { privacy }
    }

    #[must_use]
    pub const fn privacy(&self) -> &LearningPrivacyPolicy {
        &self.privacy
    }

    #[must_use]
    pub const fn capture_enabled(&self) -> bool {
        self.privacy.capture_enabled
    }

    #[must_use]
    pub const fn automatic_proposals_enabled(&self) -> bool {
        self.privacy.capture_enabled && self.privacy.automatic_proposals_enabled
    }

    #[must_use]
    pub const fn user_persistence_enabled(&self) -> bool {
        self.privacy.capture_enabled && self.privacy.user_persistence_enabled
    }

    #[must_use]
    pub const fn cross_repository_reuse_enabled(&self) -> bool {
        self.user_persistence_enabled() && self.privacy.cross_repository_reuse_enabled
    }

    #[must_use]
    pub const fn telemetry_enabled(&self) -> bool {
        self.privacy.capture_enabled && self.privacy.telemetry_enabled
    }
}

fn policy_error(message: String) -> MedusaError {
    MedusaError::new(ErrorCode::PersistenceFailed, ErrorCategory::Policy, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_is_the_outer_gate() {
        let policy = LearningAdmissionPolicy::from_privacy(LearningPrivacyPolicy {
            capture_enabled: false,
            user_persistence_enabled: true,
            cross_repository_reuse_enabled: true,
            telemetry_enabled: true,
            automatic_proposals_enabled: true,
        });
        assert!(!policy.automatic_proposals_enabled());
        assert!(!policy.user_persistence_enabled());
        assert!(!policy.cross_repository_reuse_enabled());
        assert!(!policy.telemetry_enabled());
    }

    #[test]
    fn corrupt_state_fails_closed() {
        let repo = std::env::temp_dir().join(format!(
            "medusa-learning-policy-{}-{}",
            std::process::id(),
            crate::SessionId::new()
        ));
        let root = repo.join(".medusa/learning-review");
        fs::create_dir_all(&root).expect("root");
        fs::write(root.join("state.json"), b"not-json").expect("state");
        assert!(LearningAdmissionPolicy::for_repository(&repo).is_err());
        let _ = fs::remove_dir_all(repo);
    }
}
