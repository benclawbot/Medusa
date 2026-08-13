//! First-class workspace model for repository-backed, ordinary-directory, and ephemeral work.
//!
//! A workspace is a bounded filesystem root, not necessarily a Git repository. Git-backed
//! workspaces retain worktree/commit semantics; directory and ephemeral workspaces use the same
//! runtime with content-addressed snapshot mutation when mutation is required.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use medusa_config::Config;

use crate::{RuntimeController, workspace_worker_manager::is_git_repository};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceKind {
    Git,
    Directory,
    Ephemeral,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Workspace {
    root: PathBuf,
    kind: WorkspaceKind,
    owned_ephemeral: bool,
}

impl Workspace {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, String> {
        let root = root.into();
        if !root.is_dir() {
            return Err(format!(
                "workspace root does not exist or is not a directory: {}",
                root.display()
            ));
        }
        let root = root.canonicalize().unwrap_or(root);
        let kind = if is_git_repository(&root) {
            WorkspaceKind::Git
        } else {
            WorkspaceKind::Directory
        };
        Ok(Self {
            root,
            kind,
            owned_ephemeral: false,
        })
    }

    pub fn ephemeral() -> Result<Self, String> {
        static SEQUENCE: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!(
            "medusa-workspace-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).map_err(|error| error.to_string())?;
        Ok(Self {
            root,
            kind: WorkspaceKind::Ephemeral,
            owned_ephemeral: true,
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn kind(&self) -> WorkspaceKind {
        self.kind
    }

    #[must_use]
    pub fn is_ephemeral(&self) -> bool {
        self.owned_ephemeral
    }

    /// Explicitly removes an owned ephemeral workspace. Persistent workspaces are never deleted.
    pub fn cleanup(self) -> Result<(), String> {
        if !self.owned_ephemeral {
            return Err("only Medusa-owned ephemeral workspaces may be cleaned up".to_owned());
        }
        if self.root.exists() {
            fs::remove_dir_all(&self.root).map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

impl RuntimeController {
    #[must_use]
    pub fn start_workspace(workspace: &Workspace) -> Self {
        Self::start(workspace.root.clone())
    }

    #[must_use]
    pub fn start_workspace_with_config(workspace: &Workspace, config: Config) -> Self {
        Self::start_with_config(workspace.root.clone(), config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_directory_is_a_supported_workspace() {
        let root = tempfile::tempdir().expect("workspace");
        fs::write(root.path().join("brief.md"), "research brief\n").expect("brief");
        let workspace = Workspace::open(root.path()).expect("open workspace");
        assert_eq!(workspace.kind(), WorkspaceKind::Directory);
        assert_eq!(workspace.root(), root.path().canonicalize().expect("canonical"));
    }

    #[test]
    fn ephemeral_workspace_has_explicit_lifecycle() {
        let workspace = Workspace::ephemeral().expect("ephemeral workspace");
        let root = workspace.root().to_path_buf();
        assert_eq!(workspace.kind(), WorkspaceKind::Ephemeral);
        assert!(workspace.is_ephemeral());
        fs::write(root.join("report.md"), "draft\n").expect("artifact");
        workspace.cleanup().expect("cleanup");
        assert!(!root.exists());
    }
}
