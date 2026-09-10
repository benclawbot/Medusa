use std::{env, fs, path::Path};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

use super::update_command::INSTALL_CHANNEL_MARKER;

/// Removes the installed `medusa` binary, repository `.medusa` state, the
/// installer channel marker, and installer/update locks.
///
/// The running daemon is asked to shut down first so state removal does not
/// race a live owner. Every step is best-effort except state removal: all
/// removals are reported, and failures are surfaced instead of silently
/// skipped.
pub(super) fn run(repo: &Path) -> MedusaResult<()> {
    super::request_daemon_shutdown(repo)?;

    let mut removed = Vec::new();
    let mut warnings = Vec::new();

    // 1. Repository state.
    let state_dir = repo.join(".medusa");
    if state_dir.exists() {
        fs::remove_dir_all(&state_dir).map_err(|error| {
            MedusaError::new(
                ErrorCode::PersistenceFailed,
                ErrorCategory::Execution,
                format!(
                    "could not remove Medusa state at {}: {error}",
                    state_dir.display()
                ),
            )
        })?;
        removed.push(format!("state {}", state_dir.display()));
    }

    // 2. Installed binary, channel marker, and locks beside it.
    if let Ok(executable) = env::current_exe() {
        let directory = executable.parent().map(Path::to_path_buf);
        if let Some(directory) = directory {
            for file in [
                INSTALL_CHANNEL_MARKER,
                ".medusa-update.lock",
                ".medusa-bootstrap.lock",
            ] {
                let path = directory.join(file);
                if path.exists() {
                    match fs::remove_file(&path) {
                        Ok(()) => removed.push(format!("{} {}", file.trim_start_matches('.'), path.display())),
                        Err(error) => warnings.push(format!(
                            "could not remove {}: {error}",
                            path.display()
                        )),
                    }
                }
            }
        }
        #[cfg(windows)]
        {
            // Windows cannot unlink a running image; the replacement helper
            // owns executable swaps there.
            warnings.push(format!(
                "leaving the running executable in place ({}); delete it after this process exits",
                executable.display()
            ));
        }
        #[cfg(not(windows))]
        {
            match fs::remove_file(&executable) {
                Ok(()) => removed.push(format!("binary {}", executable.display())),
                Err(error) => warnings.push(format!(
                    "could not remove binary {}: {error}",
                    executable.display()
                )),
            }
        }
    }

    if removed.is_empty() && warnings.is_empty() {
        println!("Nothing to uninstall: no Medusa state, binary, marker, or locks were found.");
    } else {
        for entry in &removed {
            println!("Removed {entry}.");
        }
    }
    for warning in &warnings {
        eprintln!("warning: {warning}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_marker_name_matches_installers() {
        assert_eq!(INSTALL_CHANNEL_MARKER, ".medusa-install-channel");
    }

    #[test]
    fn uninstall_removes_repo_state_but_keeps_repo() {
        let repo = tempfile::tempdir().expect("tempdir");
        let state = repo.path().join(".medusa");
        fs::create_dir_all(state.join("sessions")).expect("state");
        fs::write(state.join("config.toml"), "marker").expect("write");
        let keep = repo.path().join("keep.txt");
        fs::write(&keep, "keep").expect("write");

        // request_daemon_shutdown is a no-op without an owner file.
        run(repo.path()).expect("uninstall");

        assert!(!repo.path().join(".medusa").exists());
        assert!(keep.exists(), "uninstall must not touch repo contents");
    }
}
