use std::{fs, path::{Path, PathBuf}};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult, storage};
use serde::{Deserialize, Serialize};

use super::{Mode, ProviderProfileCatalog};

const PERMISSION_MODE_SCHEMA_VERSION: u32 = 1;
const PERMISSION_MODE_FILE: &str = "permissions.toml";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PermissionModeDocument {
    schema_version: u32,
    mode: Mode,
}

/// Durable, cross-frontend permission preference.
///
/// The user store is shared by the TUI and Desktop. A missing file intentionally resolves to
/// `Mode::Yolo` (displayed as Full Access), which is Medusa's fresh-install default.
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

    pub fn load(&self) -> MedusaResult<Mode> {
        if !self.path.exists() {
            return Ok(Mode::Yolo);
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
        Ok(document.mode)
    }

    pub fn save(&self, mode: Mode) -> MedusaResult<()> {
        let document = PermissionModeDocument {
            schema_version: PERMISSION_MODE_SCHEMA_VERSION,
            mode,
        };
        let text = toml::to_string_pretty(&document)
            .map_err(|error| store_error(format!("serialize permission mode: {error}")))?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                store_error(format!("create {}: {error}", parent.display()))
            })?;
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
        assert_eq!(store.load().expect("load"), Mode::Yolo);
    }

    #[test]
    fn permission_mode_survives_store_recreation() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("permissions.toml");
        PermissionModeStore::at(&path)
            .save(Mode::ApproveForMe)
            .expect("save");
        assert_eq!(
            PermissionModeStore::at(path).load().expect("reload"),
            Mode::ApproveForMe
        );
    }
}
