//! Persistent permission profiles shared by Medusa's TUI, Desktop, and tool runtime.

use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

const SCHEMA_VERSION: u32 = 1;
const FILE_NAME: &str = "permissions.toml";

/// User-facing permission profile. Labels and semantics intentionally match Codex.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionMode {
    /// Unrestricted filesystem and internet access without routine approval prompts.
    #[default]
    FullAccess,
    /// Workspace access with explicit user review for protected boundary actions.
    AskForApproval,
    /// Workspace access with automatic review of routine permission requests.
    ApproveForMe,
    /// Read-only workspace access; edits and network require approval.
    ReadOnly,
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
            Self::FullAccess => {
                "Unrestricted access to the internet and files on this computer."
            }
            Self::AskForApproval => {
                "Ask before changing files outside the workspace or using the internet."
            }
            Self::ApproveForMe => "Only ask for actions detected as potentially unsafe.",
            Self::ReadOnly => {
                "Read files in the workspace; ask before edits or internet access."
            }
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

    pub fn parse(value: &str) -> Result<Self, PermissionError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "full-access" | "full" | "yolo" => Ok(Self::FullAccess),
            "ask-for-approval" | "ask" | "review" => Ok(Self::AskForApproval),
            "approve-for-me" | "auto-review" | "auto" => Ok(Self::ApproveForMe),
            "read-only" | "readonly" | "plan" => Ok(Self::ReadOnly),
            other => Err(PermissionError(format!(
                "unknown permission mode `{other}`"
            ))),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PermissionDocument {
    schema_version: u32,
    mode: PermissionMode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionStore {
    path: PathBuf,
}

impl PermissionStore {
    pub fn user() -> Result<Self, PermissionError> {
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

    /// Missing state is Full Access by design. Once a user selects another mode the file is
    /// durable and remains authoritative across new sessions and application restarts.
    pub fn load(&self) -> Result<PermissionMode, PermissionError> {
        if !self.path.exists() {
            return Ok(PermissionMode::FullAccess);
        }
        let text = fs::read_to_string(&self.path)
            .map_err(|error| PermissionError(format!("read {}: {error}", self.path.display())))?;
        let document: PermissionDocument = toml::from_str(&text)
            .map_err(|error| PermissionError(format!("parse {}: {error}", self.path.display())))?;
        if document.schema_version != SCHEMA_VERSION {
            return Err(PermissionError(format!(
                "unsupported permission schema version {}",
                document.schema_version
            )));
        }
        Ok(document.mode)
    }

    pub fn save(&self, mode: PermissionMode) -> Result<(), PermissionError> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| PermissionError("permission path has no parent".to_owned()))?;
        fs::create_dir_all(parent)
            .map_err(|error| PermissionError(format!("create {}: {error}", parent.display())))?;
        let document = PermissionDocument {
            schema_version: SCHEMA_VERSION,
            mode,
        };
        let text = toml::to_string_pretty(&document)
            .map_err(|error| PermissionError(format!("serialize permission mode: {error}")))?;
        let temporary = self.path.with_extension("toml.tmp");
        let mut file = fs::File::create(&temporary)
            .map_err(|error| PermissionError(format!("write {}: {error}", temporary.display())))?;
        file.write_all(text.as_bytes())
            .map_err(|error| PermissionError(format!("write {}: {error}", temporary.display())))?;
        file.sync_all()
            .map_err(|error| PermissionError(format!("sync {}: {error}", temporary.display())))?;
        fs::rename(&temporary, &self.path)
            .map_err(|error| PermissionError(format!("replace {}: {error}", self.path.display())))?;
        sync_parent(&self.path);
        Ok(())
    }
}

fn user_configuration_directory() -> Result<PathBuf, PermissionError> {
    let base = if cfg!(windows) {
        env::var_os("APPDATA").map(PathBuf::from)
    } else if let Some(path) = env::var_os("XDG_CONFIG_HOME") {
        Some(PathBuf::from(path))
    } else {
        env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
    }
    .ok_or_else(|| PermissionError("could not resolve the user configuration directory".to_owned()))?;
    Ok(base.join("medusa"))
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionError(pub String);

impl std::fmt::Display for PermissionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for PermissionError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_store_defaults_to_full_access() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = PermissionStore::at(directory.path().join(FILE_NAME));
        assert_eq!(store.load().expect("load"), PermissionMode::FullAccess);
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
    fn labels_and_order_match_codex_style_menu() {
        assert_eq!(PermissionMode::ALL[0].label(), "Ask for approval");
        assert_eq!(PermissionMode::ALL[1].label(), "Approve for me");
        assert_eq!(PermissionMode::ALL[2].label(), "Full Access");
        assert_eq!(PermissionMode::ALL[3].label(), "Read Only");
        assert_eq!(PermissionMode::default(), PermissionMode::FullAccess);
    }
}
