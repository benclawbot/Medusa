use std::path::{Path, PathBuf};

/// Filesystem layout for one repository-scoped daemon instance.
#[derive(Clone, Debug)]
pub struct DaemonPaths {
    pub repo: PathBuf,
    pub directory: PathBuf,
    /// Unix-domain socket path on Unix and a loopback endpoint descriptor on Windows.
    pub socket: PathBuf,
    pub state: PathBuf,
    pub owner: PathBuf,
}

impl DaemonPaths {
    #[must_use]
    pub fn for_repo(repo: &Path) -> Self {
        let directory = repo.join(".medusa/daemon");
        Self {
            repo: repo.to_path_buf(),
            socket: directory.join("medusa.sock"),
            state: directory.join("jobs.json"),
            owner: directory.join("owner.pid"),
            directory,
        }
    }
}
