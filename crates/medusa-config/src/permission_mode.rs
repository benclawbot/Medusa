use std::{
    fs,
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult, storage};
use serde::{Deserialize, Serialize};

use super::{Mode, ProviderProfileCatalog};

const PERMISSION_MODE_SCHEMA_VERSION: u32 = 1;
const PERMISSION_MODE_FILE: &str = "permissions.toml";

/// User-facing permission profile shared by the TUI and Desktop.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionMode {
    FullAccess,
    AskForApproval,
    ApproveForMe,
    ReadOnly,
}

impl PermissionMode {
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
            Self::FullAccess => {
                "Unrestricted filesystem and internet access without routine approval prompts."
            }
            Self::AskForApproval => {
                "Work in the current workspace and ask before protected boundary actions."
            }
            Self::ApproveForMe => {
                "Work in the current workspace and auto-review routine permission requests."
            }
            Self::ReadOnly => "Read workspace files; ask before edits or internet access.",
        }
    }

    /// Existing execution modes remain the low-level compatibility contract. The additional
    /// distinction between explicit and automatic approval is handled by the permission policy.
    #[must_use]
    pub const fn execution_mode(self) -> Mode {
        match self {
            Self::FullAccess => Mode::Yolo,
            Self::AskForApproval | Self::ApproveForMe => Mode::Review,
            Self::ReadOnly => Mode::ReadOnly,
        }
    }

    #[must_use]
    pub const fn automatically_approves_routine_boundaries(self) -> bool {
        matches!(self, Self::FullAccess | Self::ApproveForMe)
    }
}

impl Default for PermissionMode {
    fn default() -> Self {
        Self::FullAccess
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PermissionModeDocument {
    schema_version: u32,
    mode: PermissionMode,
}

/// Durable, cross-frontend permission preference.
///
/// A missing preference intentionally resolves to Full Access, the requested fresh-install
/// default. The preference is not session state and therefore survives TUI/Desktop restarts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionModeStore {
    path: PathBuf,
}

impl PermissionModeStore {
    pub fn user() -> MedusaResult<Self> {
        let catalog = ProviderProfileCatalog::user()?;
        Ok(Self::at(catalog.root().join(PERMISSION_MODE_FILE)))
    }

    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> MedusaResult<PermissionMode> {
        Ok(self.load_optional()?.unwrap_or_default())
    }

    pub fn load_optional(&self) -> MedusaResult<Option<PermissionMode>> {
        if !self.path.exists() {
            return Ok(None);
        }
        let text = fs::read_to_string(&self.path)
            .map_err(|error| store_error(format!("read {}: {error}", self.path.display())))?;
        let document: PermissionModeDocument = toml::from_str(&text)
            .map_err(|error| store_error(format!("parse {}: {error}", self.path.display())))?;
        if document.schema_version != PERMISSION_MODE_SCHEMA_VERSION {
            return Err(store_error(format!(
                "unsupported permission-mode schema version {}",
                document.schema_version
            )));
        }
        Ok(Some(document.mode))
    }

    pub fn save(&self, mode: PermissionMode) -> MedusaResult<()> {
        let document = PermissionModeDocument {
            schema_version: PERMISSION_MODE_SCHEMA_VERSION,
            mode,
        };
        let text = toml::to_string_pretty(&document)
            .map_err(|error| store_error(format!("serialize permission mode: {error}")))?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| store_error(format!("create {}: {error}", parent.display())))?;
        }
        storage::write_atomic(&self.path, text.as_bytes())
            .map_err(|error| store_error(format!("write {}: {error}", self.path.display())))
    }
}

fn store_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::PersistenceFailed,
        ErrorCategory::Persistence,
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_preference_defaults_to_full_access() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = PermissionModeStore::at(directory.path().join("permissions.toml"));
        assert_eq!(store.load().expect("load"), PermissionMode::FullAccess);
        assert_eq!(store.load().expect("load").execution_mode(), Mode::Yolo);
    }

    #[test]
    fn permission_mode_survives_store_recreation() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("permissions.toml");
        PermissionModeStore::at(&path)
            .save(PermissionMode::ApproveForMe)
            .expect("save");
        assert_eq!(
            PermissionModeStore::at(path).load().expect("reload"),
            PermissionMode::ApproveForMe
        );
    }

    #[test]
    fn codex_style_profiles_map_to_existing_execution_modes() {
        assert_eq!(PermissionMode::FullAccess.execution_mode(), Mode::Yolo);
        assert_eq!(
            PermissionMode::AskForApproval.execution_mode(),
            Mode::Review
        );
        assert_eq!(PermissionMode::ApproveForMe.execution_mode(), Mode::Review);
        assert_eq!(PermissionMode::ReadOnly.execution_mode(), Mode::ReadOnly);
        assert!(PermissionMode::ApproveForMe.automatically_approves_routine_boundaries());
    }
}
