use std::{
    env, fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::Command,
};

use flate2::read::GzDecoder;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

use crate::model::invalid;

/// Whether a binary may be self-replaced or is owned by a package manager.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstallKind {
    SelfManaged,
    PackageManaged {
        manager: &'static str,
        command: &'static str,
    },
}

/// Location and ownership of the running executable.
#[derive(Clone, Debug)]
pub struct InstallLocation {
    pub executable: PathBuf,
    pub kind: InstallKind,
}

impl InstallLocation {
    pub fn current() -> MedusaResult<Self> {
        let executable = env::current_exe()?;
        Ok(Self {
            kind: package_manager_for(&executable),
            executable,
        })
    }
}

/// Request to start a fresh process after a successful update.
#[derive(Clone, Debug, Default)]
pub struct Restart {
    pub arguments: Vec<String>,
}

impl Restart {
    pub fn spawn(&self, executable: &Path) -> MedusaResult<()> {
        Command::new(executable)
            .args(&self.arguments)
            .spawn()
            .map(|_| ())
            .map_err(io_error)
    }
}

/// Extracts exactly one Medusa executable and swaps it with a rollback backup.
#[derive(Clone, Debug)]
pub struct AtomicInstaller {
    target: PathBuf,
}

impl AtomicInstaller {
    #[must_use]
    pub fn new(target: PathBuf) -> Self {
        Self { target }
    }

    pub fn extract_archive(&self, archive: &Path, workspace: &Path) -> MedusaResult<PathBuf> {
        fs::create_dir_all(workspace)?;
        let extension = archive.to_string_lossy().to_ascii_lowercase();
        if extension.ends_with(".zip") {
            extract_zip(archive, workspace)
        } else if extension.ends_with(".tar.gz") || extension.ends_with(".tgz") {
            extract_tar_gz(archive, workspace)
        } else {
            Err(invalid("unsupported update archive format"))
        }
    }

    /// Replaces a Unix binary atomically and restores the prior binary if the move fails.
    /// Windows schedules replacement after this process exits, avoiding executable locking.
    pub fn replace(&self, candidate: &Path, restart: &Restart) -> MedusaResult<Option<PathBuf>> {
        validate_candidate(candidate)?;
        if cfg!(windows) {
            self.schedule_windows_replace(candidate, restart)?;
            return Ok(None);
        }
        let backup = self.target.with_extension("previous");
        if backup.exists() {
            fs::remove_file(&backup)?;
        }
        if self.target.exists() {
            fs::rename(&self.target, &backup)?;
        }
        match fs::rename(candidate, &self.target) {
            Ok(()) => {
                #[cfg(unix)]
                set_executable(&self.target)?;
                restart.spawn(&self.target)?;
                Ok(Some(backup))
            }
            Err(error) => {
                if backup.exists() {
                    let _ = fs::rename(&backup, &self.target);
                }
                Err(io_error(error))
            }
        }
    }

    #[cfg(windows)]
    fn schedule_windows_replace(&self, candidate: &Path, restart: &Restart) -> MedusaResult<()> {
        let script = self.target.with_extension("update.cmd");
        let backup = self.target.with_extension("previous.exe");
        let staged = self.target.with_extension("update-new.exe");
        // The download lives in a temporary directory. Copy it beside the
        // running executable before spawning the delayed helper so cleanup
        // cannot race the Windows file-lock workaround.
        fs::copy(candidate, &staged)?;
        let content =
            windows_replace_script(std::process::id(), &backup, &self.target, &staged, restart);
        fs::write(&script, content)?;
        Command::new("cmd")
            .args(["/C", "start", "", "/B"])
            .arg(&script)
            .spawn()
            .map(|_| ())
            .map_err(io_error)
    }

    #[cfg(not(windows))]
    fn schedule_windows_replace(&self, _candidate: &Path, _restart: &Restart) -> MedusaResult<()> {
        unreachable!("windows replacement is only selected on Windows")
    }
}

fn windows_replace_script(
    parent_pid: u32,
    backup: &Path,
    target: &Path,
    candidate: &Path,
    restart: &Restart,
) -> String {
    let restart_args = restart
        .arguments
        .iter()
        .map(|argument| format!("\"{}\"", argument.replace('"', "\\\"")))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "@echo off\r\nsetlocal\r\n:wait_for_parent\r\ntasklist /fi \"PID eq {parent_pid}\" /nh | find \"{parent_pid}\" >nul\r\nif not errorlevel 1 (\r\n  timeout /t 1 /nobreak >nul\r\n  goto wait_for_parent\r\n)\r\nif exist \"{backup}\" del /f /q \"{backup}\"\r\nif exist \"{target}\" move /y \"{target}\" \"{backup}\"\r\nmove /y \"{candidate}\" \"{target}\"\r\nstart \"\" \"{target}\" {restart_args}\r\ndel /f /q \"%~f0\"\r\n",
        backup = backup.display(),
        target = target.display(),
        candidate = candidate.display(),
    )
}

fn extract_zip(archive: &Path, workspace: &Path) -> MedusaResult<PathBuf> {
    let file = fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file).map_err(zip_error)?;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(zip_error)?;
        let Some(name) = Path::new(entry.name()).file_name() else {
            continue;
        };
        if !is_medusa_binary(name) || entry.is_dir() {
            continue;
        }
        let target = workspace.join(name);
        copy_entry(&mut entry, &target)?;
        return Ok(target);
    }
    Err(invalid(
        "update archive does not contain a Medusa executable",
    ))
}

fn extract_tar_gz(archive: &Path, workspace: &Path) -> MedusaResult<PathBuf> {
    let file = fs::File::open(archive)?;
    let mut archive = tar::Archive::new(GzDecoder::new(file));
    let mut entries = archive.entries().map_err(io_error)?;
    for entry in entries.by_ref() {
        let mut entry = entry.map_err(io_error)?;
        let path = entry.path().map_err(io_error)?;
        let Some(name) = path.file_name() else {
            continue;
        };
        if !is_medusa_binary(name) || !entry.header().entry_type().is_file() {
            continue;
        }
        let target = workspace.join(name);
        copy_entry(&mut entry, &target)?;
        return Ok(target);
    }
    Err(invalid(
        "update archive does not contain a Medusa executable",
    ))
}

fn copy_entry(reader: &mut impl Read, target: &Path) -> MedusaResult<()> {
    let mut output = fs::File::create(target)?;
    io::copy(reader, &mut output)?;
    Ok(())
}

fn is_medusa_binary(name: &std::ffi::OsStr) -> bool {
    matches!(name.to_string_lossy().as_ref(), "medusa" | "medusa.exe")
}

fn validate_candidate(candidate: &Path) -> MedusaResult<()> {
    let metadata = fs::metadata(candidate)?;
    if metadata.is_file() && metadata.len() > 0 {
        Ok(())
    } else {
        Err(invalid("update candidate is missing or empty"))
    }
}

fn package_manager_for(executable: &Path) -> InstallKind {
    if cfg!(target_os = "macos") && executable.to_string_lossy().contains("/Cellar/") {
        return InstallKind::PackageManaged {
            manager: "Homebrew",
            command: "brew upgrade medusa",
        };
    }
    if cfg!(target_os = "linux") && executable.starts_with("/usr/bin") {
        return InstallKind::PackageManaged {
            manager: "system package manager",
            command: "sudo apt update && sudo apt install medusa",
        };
    }
    InstallKind::SelfManaged
}

#[cfg(unix)]
fn set_executable(path: &Path) -> MedusaResult<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn io_error(error: impl std::fmt::Display) -> MedusaError {
    MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Execution,
        error.to_string(),
    )
}

fn zip_error(error: impl std::fmt::Display) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        format!("invalid update ZIP: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Platform;

    #[test]
    fn rejects_empty_update_candidate_without_touching_target() {
        let directory = tempfile::tempdir().expect("tempdir");
        let target = directory.path().join("medusa");
        let candidate = directory.path().join("candidate");
        fs::write(&target, b"old").expect("target");
        fs::write(&candidate, b"").expect("candidate");
        assert!(
            AtomicInstaller::new(target.clone())
                .replace(&candidate, &Restart::default())
                .is_err()
        );
        assert_eq!(fs::read(&target).expect("target preserved"), b"old");
    }

    #[test]
    fn platform_asset_names_are_stable() {
        assert_eq!(
            Platform {
                os: "windows".into(),
                architecture: "x86_64".into()
            }
            .cli_asset_name(),
            "medusa-cli-windows.zip"
        );
    }

    #[test]
    fn windows_handoff_waits_for_the_running_binary_before_replacement() {
        let restart = Restart {
            arguments: vec!["resume".into(), "session-1".into()],
        };
        let script = windows_replace_script(
            4242,
            Path::new(r"C:\bin\medusa.previous.exe"),
            Path::new(r"C:\bin\medusa.exe"),
            Path::new(r"C:\bin\medusa.update-new.exe"),
            &restart,
        );
        assert!(script.contains(":wait_for_parent"));
        assert!(script.contains("tasklist /fi \"PID eq 4242\""));
        assert!(script.contains("goto wait_for_parent"));
        assert!(script.contains("move /y \"C:\\bin\\medusa.update-new.exe\""));
        assert!(script.contains("start \"\" \"C:\\bin\\medusa.exe\" \"resume\" \"session-1\""));
    }
}
