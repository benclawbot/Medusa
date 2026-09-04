use std::{
    fs::{self, OpenOptions},
    io::{self, Read as _, Write as _},
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_process_containment::process_start_marker;
use sha2::{Digest, Sha256};

use crate::install::{AtomicInstaller as LegacyAtomicInstaller, Restart, ScheduledUpdate};

mod helper;

#[cfg(test)]
mod tests;

const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const UPDATE_LOCK_SCHEMA: &str = "3";

/// Windows self-update facade. Archive extraction remains shared with the
/// cross-platform installer; the final handoff is Windows-specific because an
/// executing `.exe` cannot replace itself.
#[derive(Clone, Debug)]
pub struct AtomicInstaller {
    target: PathBuf,
    legacy: LegacyAtomicInstaller,
}

impl AtomicInstaller {
    #[must_use]
    pub fn new(target: PathBuf) -> Self {
        Self {
            legacy: LegacyAtomicInstaller::new(target.clone()),
            target,
        }
    }

    pub fn extract_archive(&self, archive: &Path, workspace: &Path) -> MedusaResult<PathBuf> {
        self.legacy.extract_archive(archive, workspace)
    }

    pub fn recover_interrupted(&self) -> MedusaResult<bool> {
        self.legacy.recover_interrupted()
    }

    pub fn schedule_replace(
        &self,
        candidate: &Path,
        restart: &Restart,
        parent_pid: u32,
    ) -> MedusaResult<ScheduledUpdate> {
        schedule_windows_health_checked_replace(&self.target, candidate, restart, parent_pid)
    }

    pub fn replace(&self, candidate: &Path, restart: &Restart) -> MedusaResult<Option<PathBuf>> {
        let update = self.schedule_replace(candidate, restart, std::process::id())?;
        Ok(Some(update.backup))
    }
}

fn schedule_windows_health_checked_replace(
    target: &Path,
    candidate: &Path,
    restart: &Restart,
    parent_pid: u32,
) -> MedusaResult<ScheduledUpdate> {
    let metadata = fs::metadata(candidate).map_err(io_error)?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(invalid("update candidate is missing or empty"));
    }
    if restart.sequence_file.is_some() != restart.rollout_sequence.is_some() {
        return Err(invalid(
            "restart sequence file and rollout sequence must be configured together",
        ));
    }
    if restart.rollout_sequence == Some(0) {
        return Err(invalid("rollout sequence must be positive"));
    }

    let directory = target
        .parent()
        .ok_or_else(|| invalid("update target has no parent directory"))?;
    let staged = target.with_extension("update-new.exe");
    let backup = target.with_extension("previous.exe");
    let helper_path = target.with_extension("update.ps1");
    let state = directory.join(".medusa-update-state");
    let health = directory.join(".medusa-update-health");
    let lock = directory.join(".medusa-update.lock");

    let mut lock_file = acquire_windows_update_lock(&lock, parent_pid)?;
    let mut helper_process = None;
    let result = (|| -> MedusaResult<()> {
        if !target.exists() && backup.exists() {
            fs::rename(&backup, target).map_err(io_error)?;
        }
        for path in [&staged, &helper_path] {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_error(error)),
            }
        }

        fs::copy(candidate, &staged).map_err(io_error)?;
        OpenOptions::new()
            .write(true)
            .open(&staged)
            .map_err(io_error)?
            .sync_all()
            .map_err(io_error)?;

        let expected_hash = sha256_file(&staged)?;
        let script = helper::windows_health_checked_replace_script(helper::WindowsReplaceScript {
            parent_pid,
            target,
            staged: &staged,
            backup: &backup,
            state: &state,
            health: &health,
            lock: &lock,
            expected_hash: &expected_hash,
            restart,
        });
        fs::write(&helper_path, script.as_bytes()).map_err(io_error)?;

        let child = Command::new("powershell")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(&helper_path)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .creation_flags(CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .map_err(io_error)?;
        let helper_pid = child.id();
        helper_process = Some(child);
        writeln!(lock_file, "helper_pid={helper_pid}").map_err(io_error)?;
        if let Some(identity) = process_identity(helper_pid)? {
            writeln!(lock_file, "helper_identity={identity}").map_err(io_error)?;
        }
        writeln!(lock_file, "helper_ready=1").map_err(io_error)?;
        lock_file.sync_all().map_err(io_error)?;
        Ok(())
    })();

    if let Err(error) = result {
        if let Some(mut child) = helper_process {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_file(&lock);
        let _ = fs::remove_file(&staged);
        let _ = fs::remove_file(&helper_path);
        return Err(error);
    }

    // A running Windows executable cannot be swapped. The helper is fully
    // staged and owns rollback before this process exits.
    std::process::exit(0)
}

fn acquire_windows_update_lock(lock: &Path, parent_pid: u32) -> MedusaResult<fs::File> {
    loop {
        match OpenOptions::new().write(true).create_new(true).open(lock) {
            Ok(mut file) => {
                let result = (|| -> MedusaResult<()> {
                    writeln!(file, "schema={UPDATE_LOCK_SCHEMA}").map_err(io_error)?;
                    writeln!(file, "parent_pid={parent_pid}").map_err(io_error)?;
                    if let Some(identity) = process_identity(parent_pid)? {
                        writeln!(file, "parent_identity={identity}").map_err(io_error)?;
                    }
                    writeln!(file, "lock_ready=1").map_err(io_error)?;
                    file.sync_all().map_err(io_error)?;
                    Ok(())
                })();
                if let Err(error) = result {
                    let _ = fs::remove_file(lock);
                    return Err(error);
                }
                return Ok(file);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if update_owner_is_alive(lock)? {
                    return Err(invalid("another Medusa update is already staged"));
                }
                match fs::remove_file(lock) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(io_error(error)),
                }
            }
            Err(error) => return Err(io_error(error)),
        }
    }
}

fn update_owner_is_alive(lock: &Path) -> MedusaResult<bool> {
    let content = match fs::read_to_string(lock) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(io_error(error)),
    };
    let Some(parent_pid) =
        lock_field(&content, "parent_pid").and_then(|value| value.parse::<u32>().ok())
    else {
        return Ok(true);
    };
    let schema = lock_field(&content, "schema");
    if schema.is_some_and(|value| value != UPDATE_LOCK_SCHEMA) {
        return Ok(true);
    }
    if schema.is_some() && lock_field(&content, "lock_ready") != Some("1") {
        return Ok(true);
    }
    if process_matches(parent_pid, lock_field(&content, "parent_identity"))? {
        return Ok(true);
    }
    if let Some(helper_value) = lock_field(&content, "helper_pid") {
        let Ok(helper_pid) = helper_value.parse::<u32>() else {
            return Ok(true);
        };
        if lock_field(&content, "helper_ready") != Some("1") {
            return Ok(true);
        }
        if process_matches(helper_pid, lock_field(&content, "helper_identity"))? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn lock_field<'a>(content: &'a str, name: &str) -> Option<&'a str> {
    content
        .lines()
        .find_map(|line| line.strip_prefix(name)?.strip_prefix('='))
}

fn process_identity(pid: u32) -> MedusaResult<Option<String>> {
    process_start_marker(pid).map_err(io_error).map(|marker| {
        marker.map(|marker| {
            format!(
                "{}|{}|{}",
                marker.platform,
                marker.value,
                marker.boot_id.unwrap_or_default()
            )
        })
    })
}

fn process_matches(pid: u32, expected_identity: Option<&str>) -> MedusaResult<bool> {
    let observed_identity = process_identity(pid)?;
    Ok(match (expected_identity, observed_identity) {
        (None, Some(_)) => true,
        (Some(expected), Some(observed)) => expected == observed,
        (_, None) => false,
    })
}

fn sha256_file(path: &Path) -> MedusaResult<String> {
    let mut file = fs::File::open(path).map_err(io_error)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(io_error)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

fn io_error(error: impl std::fmt::Display) -> MedusaError {
    MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Execution,
        error.to_string(),
    )
}
