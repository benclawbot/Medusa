use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};

use crate::Mode;

const SCHEMA_VERSION: u32 = 1;
const FILE_NAME: &str = "permissions.toml";

/// User-facing permission profile shared by Medusa's TUI and Desktop.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionMode {
    /// Unrestricted filesystem and internet access without routine approval prompts.
    FullAccess,
    /// Workspace access with explicit user review for protected boundary actions.
    #[default]
    AskForApproval,
    /// Workspace access with automatic review of routine permission requests.
    ApproveForMe,
    /// Read-only workspace access; edits and network require approval.
    ReadOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApprovalPolicy {
    Never,
    OnBoundary,
    UnsafeOnly,
    OnMutationOrNetwork,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SandboxProfile {
    Unrestricted,
    WorkspaceWrite,
    ReadOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewerPolicy {
    BypassRoutine,
    RequireUser,
    AutoReviewRoutine,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PermissionContext {
    pub execution_mode: Mode,
    pub approval_policy: ApprovalPolicy,
    pub sandbox_profile: SandboxProfile,
    pub reviewer_policy: ReviewerPolicy,
}

impl PermissionMode {
    pub const ALL: [Self; 4] = [
        Self::AskForApproval,
        Self::ApproveForMe,
        Self::FullAccess,
        Self::ReadOnly,
    ];

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::FullAccess => "full-access",
            Self::AskForApproval => "ask-for-approval",
            Self::ApproveForMe => "approve-for-me",
            Self::ReadOnly => "read-only",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::FullAccess => "Full Access",
            Self::AskForApproval => "Ask for approval",
            Self::ApproveForMe => "Approve for me",
            Self::ReadOnly => "Read Only",
        }
    }

    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::FullAccess => "Unrestricted access to the internet and files on this computer.",
            Self::AskForApproval => {
                "Ask before changing files outside the workspace or using the internet."
            }
            Self::ApproveForMe => "Only ask for actions detected as potentially unsafe.",
            Self::ReadOnly => "Read files in the workspace; ask before edits or internet access.",
        }
    }

    /// Map the richer UI permission profile to the existing low-level execution mode.
    #[must_use]
    pub const fn execution_mode(self) -> Mode {
        match self {
            Self::FullAccess => Mode::Yolo,
            Self::AskForApproval | Self::ApproveForMe => Mode::Review,
            Self::ReadOnly => Mode::ReadOnly,
        }
    }

    /// Resolve the complete permission context in one value so callers cannot update only one
    /// safety dimension when switching profiles.
    #[must_use]
    pub const fn context(self) -> PermissionContext {
        match self {
            Self::FullAccess => PermissionContext {
                execution_mode: Mode::Yolo,
                approval_policy: ApprovalPolicy::Never,
                sandbox_profile: SandboxProfile::Unrestricted,
                reviewer_policy: ReviewerPolicy::BypassRoutine,
            },
            Self::AskForApproval => PermissionContext {
                execution_mode: Mode::Review,
                approval_policy: ApprovalPolicy::OnBoundary,
                sandbox_profile: SandboxProfile::WorkspaceWrite,
                reviewer_policy: ReviewerPolicy::RequireUser,
            },
            Self::ApproveForMe => PermissionContext {
                execution_mode: Mode::Review,
                approval_policy: ApprovalPolicy::UnsafeOnly,
                sandbox_profile: SandboxProfile::WorkspaceWrite,
                reviewer_policy: ReviewerPolicy::AutoReviewRoutine,
            },
            Self::ReadOnly => PermissionContext {
                execution_mode: Mode::ReadOnly,
                approval_policy: ApprovalPolicy::OnMutationOrNetwork,
                sandbox_profile: SandboxProfile::ReadOnly,
                reviewer_policy: ReviewerPolicy::RequireUser,
            },
        }
    }

    #[must_use]
    pub const fn auto_approves_routine_boundaries(self) -> bool {
        matches!(self, Self::FullAccess | Self::ApproveForMe)
    }

    #[must_use]
    pub const fn is_read_only(self) -> bool {
        matches!(self, Self::ReadOnly)
    }

    pub fn parse(value: &str) -> MedusaResult<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "full-access" | "full" | "yolo" => Ok(Self::FullAccess),
            "ask-for-approval" | "ask" | "review" => Ok(Self::AskForApproval),
            "approve-for-me" | "auto-review" | "auto" => Ok(Self::ApproveForMe),
            "read-only" | "readonly" | "plan" => Ok(Self::ReadOnly),
            other => Err(config_error(format!("unknown permission mode `{other}`"))),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PermissionDocument {
    schema_version: u32,
    mode: PermissionMode,
}

/// Durable cross-frontend permission preference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionStore {
    path: PathBuf,
}

impl PermissionStore {
    pub fn user() -> MedusaResult<Self> {
        Ok(Self::at(user_configuration_directory()?.join(FILE_NAME)))
    }

    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Missing state is a first-run state and resolves to the least-privilege interactive mode.
    /// Persisted selections, including Full Access from existing users, remain unchanged.
    pub fn load(&self) -> MedusaResult<PermissionMode> {
        if !self.path.exists() {
            return Ok(PermissionMode::AskForApproval);
        }
        let text = fs::read_to_string(&self.path)
            .map_err(|error| store_error(format!("read {}: {error}", self.path.display())))?;
        let document: PermissionDocument = toml::from_str(&text)
            .map_err(|error| store_error(format!("parse {}: {error}", self.path.display())))?;
        if document.schema_version != SCHEMA_VERSION {
            return Err(store_error(format!(
                "unsupported permission schema version {}",
                document.schema_version
            )));
        }
        Ok(document.mode)
    }

    pub fn save(&self, mode: PermissionMode) -> MedusaResult<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| store_error("permission path has no parent"))?;
        fs::create_dir_all(parent)
            .map_err(|error| store_error(format!("create {}: {error}", parent.display())))?;
        let text = toml::to_string_pretty(&PermissionDocument {
            schema_version: SCHEMA_VERSION,
            mode,
        })
        .map_err(|error| store_error(format!("serialize permission mode: {error}")))?;
        let temporary = self.path.with_extension("toml.tmp");
        let mut file = fs::File::create(&temporary)
            .map_err(|error| store_error(format!("write {}: {error}", temporary.display())))?;
        file.write_all(text.as_bytes())
            .map_err(|error| store_error(format!("write {}: {error}", temporary.display())))?;
        file.sync_all()
            .map_err(|error| store_error(format!("sync {}: {error}", temporary.display())))?;
        fs::rename(&temporary, &self.path)
            .map_err(|error| store_error(format!("replace {}: {error}", self.path.display())))?;
        sync_parent(&self.path);
        Ok(())
    }
}

fn user_configuration_directory() -> MedusaResult<PathBuf> {
    let base = if cfg!(windows) {
        env::var_os("APPDATA").map(PathBuf::from)
    } else if let Some(path) = env::var_os("XDG_CONFIG_HOME") {
        Some(PathBuf::from(path))
    } else {
        env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
    }
    .ok_or_else(|| config_error("could not resolve the user configuration directory"))?;
    Ok(base.join("medusa"))
}

fn config_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Environment,
        message,
    )
}

fn store_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::PersistenceFailed,
        ErrorCategory::Persistence,
        message,
    )
}

fn sync_parent(path: &Path) {
    #[cfg(unix)]
    if let Some(parent) = path.parent()
        && let Ok(directory) = fs::File::open(parent)
    {
        let _ = directory.sync_all();
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_store_defaults_to_ask_for_approval() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = PermissionStore::at(directory.path().join(FILE_NAME));
        assert_eq!(store.load().expect("load"), PermissionMode::AskForApproval);
        assert_eq!(store.load().expect("load").execution_mode(), Mode::Review);
    }

    #[test]
    fn selection_survives_store_recreation() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join(FILE_NAME);
        PermissionStore::at(&path)
            .save(PermissionMode::ApproveForMe)
            .expect("save");
        assert_eq!(
            PermissionStore::at(path).load().expect("reload"),
            PermissionMode::ApproveForMe
        );
    }

    #[test]
    fn persisted_full_access_survives_default_migration() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join(FILE_NAME);
        PermissionStore::at(&path)
            .save(PermissionMode::FullAccess)
            .expect("save");
        assert_eq!(
            PermissionStore::at(path).load().expect("reload"),
            PermissionMode::FullAccess
        );
    }

    #[test]
    fn labels_and_order_match_codex_style_menu() {
        assert_eq!(PermissionMode::ALL[0].label(), "Ask for approval");
        assert_eq!(PermissionMode::ALL[1].label(), "Approve for me");
        assert_eq!(PermissionMode::ALL[2].label(), "Full Access");
        assert_eq!(PermissionMode::ALL[3].label(), "Read Only");
        assert_eq!(PermissionMode::default(), PermissionMode::AskForApproval);
    }

    #[test]
    fn profiles_resolve_atomic_permission_contexts() {
        assert_eq!(
            PermissionMode::FullAccess.context(),
            PermissionContext {
                execution_mode: Mode::Yolo,
                approval_policy: ApprovalPolicy::Never,
                sandbox_profile: SandboxProfile::Unrestricted,
                reviewer_policy: ReviewerPolicy::BypassRoutine,
            }
        );
        assert_eq!(
            PermissionMode::AskForApproval.context(),
            PermissionContext {
                execution_mode: Mode::Review,
                approval_policy: ApprovalPolicy::OnBoundary,
                sandbox_profile: SandboxProfile::WorkspaceWrite,
                reviewer_policy: ReviewerPolicy::RequireUser,
            }
        );
        assert_eq!(
            PermissionMode::ApproveForMe.context(),
            PermissionContext {
                execution_mode: Mode::Review,
                approval_policy: ApprovalPolicy::UnsafeOnly,
                sandbox_profile: SandboxProfile::WorkspaceWrite,
                reviewer_policy: ReviewerPolicy::AutoReviewRoutine,
            }
        );
        assert_eq!(
            PermissionMode::ReadOnly.context(),
            PermissionContext {
                execution_mode: Mode::ReadOnly,
                approval_policy: ApprovalPolicy::OnMutationOrNetwork,
                sandbox_profile: SandboxProfile::ReadOnly,
                reviewer_policy: ReviewerPolicy::RequireUser,
            }
        );
    }
}
