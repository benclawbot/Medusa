use std::{
    env, fs,
    fs::OpenOptions,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    process::Command,
};

use flate2::read::GzDecoder;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

use crate::model::invalid;

pub const HEALTH_FILE_ENV: &str = "MEDUSA_UPDATE_HEALTH_FILE";

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

/// Request to restart the same user-visible session after a healthy update.
#[derive(Clone, Debug, Default)]
pub struct Restart {
    pub arguments: Vec<String>,
    /// Commit this rollout sequence only after the replacement acknowledges startup.
    pub sequence_file: Option<PathBuf>,
    pub rollout_sequence: Option<u64>,
}

/// Paths retained by the detached replacement helper.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduledUpdate {
    pub helper: PathBuf,
    pub backup: PathBuf,
    pub state: PathBuf,
    pub health: PathBuf,
}

/// Acknowledges that a newly started binary completed its startup path.
pub fn acknowledge_update_health() -> MedusaResult<bool> {
    let Some(path) = env::var_os(HEALTH_FILE_ENV).map(PathBuf::from) else {
        return Ok(false);
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    atomic_write(&path, b"healthy\n")?;
    Ok(true)
}

/// Extracts exactly one confined Medusa executable and schedules an atomic swap.
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
        let candidate = if extension.ends_with(".zip") {
            extract_zip(archive, workspace)?
        } else if extension.ends_with(".tar.gz") || extension.ends_with(".tgz") {
            extract_tar_gz(archive, workspace)?
        } else {
            return Err(invalid("unsupported update archive format"));
        };
        validate_candidate(&candidate)?;
        Ok(candidate)
    }

    /// Restores an interrupted swap when the target is absent and a backup exists.
    pub fn recover_interrupted(&self) -> MedusaResult<bool> {
        let backup = backup_path(&self.target);
        if !self.target.exists() && backup.exists() {
            fs::rename(&backup, &self.target)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Stages the candidate beside the running executable and starts a detached helper.
    /// The helper waits for this process to exit, swaps atomically, requires a startup
    /// health marker, and restores the backup if the new process exits or times out.
    pub fn schedule_replace(
        &self,
        candidate: &Path,
        restart: &Restart,
        parent_pid: u32,
    ) -> MedusaResult<ScheduledUpdate> {
        validate_candidate(candidate)?;
        if restart.sequence_file.is_some() != restart.rollout_sequence.is_some() {
            return Err(invalid(
                "restart sequence file and rollout sequence must be configured together",
            ));
        }
        if restart.rollout_sequence == Some(0) {
            return Err(invalid("rollout sequence must be positive"));
        }
        self.recover_interrupted()?;
        let directory = self
            .target
            .parent()
            .ok_or_else(|| invalid("update target has no parent directory"))?;
        let lock = directory.join(".medusa-update.lock");
        let mut lock_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .map_err(|error| {
                if error.kind() == io::ErrorKind::AlreadyExists {
                    invalid("another Medusa update is already staged")
                } else {
                    io_error(error)
                }
            })?;
        writeln!(lock_file, "parent_pid={parent_pid}")?;
        lock_file.sync_all()?;

        let staged = staged_path(&self.target);
        let backup = backup_path(&self.target);
        let state = directory.join(".medusa-update-state");
        let health = directory.join(".medusa-update-health");
        let helper = helper_path(&self.target);
        let result = (|| -> MedusaResult<()> {
            if staged.exists() {
                fs::remove_file(&staged)?;
            }
            fs::copy(candidate, &staged)?;
            sync_staged_copy(&staged)?;
            validate_candidate(&staged)?;
            #[cfg(unix)]
            set_executable(&staged)?;
            let script = if cfg!(windows) {
                windows_replace_script(
                    parent_pid,
                    &backup,
                    &self.target,
                    &staged,
                    &state,
                    &health,
                    &lock,
                    restart,
                )
            } else {
                unix_replace_script(
                    parent_pid,
                    &backup,
                    &self.target,
                    &staged,
                    &state,
                    &health,
                    &lock,
                    restart,
                )
            };
            atomic_write(&helper, script.as_bytes())?;
            #[cfg(unix)]
            set_executable(&helper)?;
            helper_command(&helper).spawn().map_err(io_error)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&lock);
            let _ = fs::remove_file(&staged);
            let _ = fs::remove_file(&helper);
        }
        result?;
        Ok(ScheduledUpdate {
            helper,
            backup,
            state,
            health,
        })
    }

    /// Compatibility wrapper. Replacement is always delayed and health checked.
    pub fn replace(&self, candidate: &Path, restart: &Restart) -> MedusaResult<Option<PathBuf>> {
        let update = self.schedule_replace(candidate, restart, std::process::id())?;
        Ok(Some(update.backup))
    }
}

fn helper_command(script: &Path) -> Command {
    if cfg!(windows) {
        let mut command = Command::new("powershell");
        command
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(script);
        command
    } else {
        let mut command = Command::new("sh");
        command.arg(script);
        command
    }
}

// Atomic replacement scripts intentionally receive every persisted path explicitly.
#[allow(clippy::too_many_arguments)]
fn unix_replace_script(
    parent_pid: u32,
    backup: &Path,
    target: &Path,
    candidate: &Path,
    state: &Path,
    health: &Path,
    lock: &Path,
    restart: &Restart,
) -> String {
    let arguments = restart
        .arguments
        .iter()
        .map(|argument| shell_quote(argument))
        .collect::<Vec<_>>()
        .join(" ");
    let sequence_file = restart
        .sequence_file
        .as_deref()
        .map(shell_quote_path)
        .unwrap_or_else(|| shell_quote(""));
    let sequence_value = restart
        .rollout_sequence
        .map(|value| shell_quote(&value.to_string()))
        .unwrap_or_else(|| shell_quote(""));
    format!(
        r##"#!/bin/sh
set -eu
parent={parent_pid}
backup={backup}
target={target}
candidate={candidate}
state={state}
health={health}
lock={lock}
sequence_file={sequence_file}
sequence_value={sequence_value}
child=''
rollback() {{
  if [ -n "$child" ]; then kill "$child" 2>/dev/null || true; fi
  rm -f "$target"
  if [ -e "$backup" ]; then mv "$backup" "$target"; fi
  printf 'rolled-back\n' > "$state"
  rm -f "$lock"
  "$target" {arguments} >/dev/null 2>&1 &
  rm -f "$0"
  exit 1
}}
while kill -0 "$parent" 2>/dev/null; do sleep 1; done
rm -f "$health" "$backup"
printf 'swapping\n' > "$state"
if [ -e "$target" ]; then mv "$target" "$backup"; fi
if ! mv "$candidate" "$target"; then
  if [ -e "$backup" ]; then mv "$backup" "$target"; fi
  printf 'swap-failed\n' > "$state"
  rm -f "$lock"
  exit 1
fi
chmod 755 "$target" || rollback
MEDUSA_UPDATE_HEALTH_FILE="$health" "$target" {arguments} &
child=$!
i=0
while [ "$i" -lt 100 ]; do
  if [ -s "$health" ]; then
    if [ -n "$sequence_file" ]; then
      printf '%s\n' "$sequence_value" > "$sequence_file.tmp" || rollback
      mv "$sequence_file.tmp" "$sequence_file" || rollback
    fi
    printf 'healthy\n' > "$state"
    rm -f "$backup" "$lock" "$0"
    exit 0
  fi
  if ! kill -0 "$child" 2>/dev/null; then break; fi
  i=$((i + 1))
  sleep 0.1
done
rollback
"##,
        backup = shell_quote_path(backup),
        target = shell_quote_path(target),
        candidate = shell_quote_path(candidate),
        state = shell_quote_path(state),
        health = shell_quote_path(health),
        lock = shell_quote_path(lock),
    )
}

// Atomic replacement scripts intentionally receive every persisted path explicitly.
#[allow(clippy::too_many_arguments)]
fn windows_replace_script(
    parent_pid: u32,
    backup: &Path,
    target: &Path,
    candidate: &Path,
    state: &Path,
    health: &Path,
    lock: &Path,
    restart: &Restart,
) -> String {
    let arguments = restart
        .arguments
        .iter()
        .map(|argument| powershell_quote(argument))
        .collect::<Vec<_>>()
        .join(", ");
    let sequence_file = restart
        .sequence_file
        .as_deref()
        .map(powershell_quote_path)
        .unwrap_or_else(|| powershell_quote(""));
    let sequence_value = restart
        .rollout_sequence
        .map(|value| powershell_quote(&value.to_string()))
        .unwrap_or_else(|| powershell_quote(""));
    format!(
        r##"$ErrorActionPreference = 'Stop'
$parentPid = {parent_pid}
$backup = {backup}
$target = {target}
$candidate = {candidate}
$state = {state}
$health = {health}
$lock = {lock}
$sequenceFile = {sequence_file}
$sequenceValue = {sequence_value}
function Restore-Previous([object]$Child) {{
  if ($null -ne $Child) {{ Stop-Process -Id $Child.Id -Force -ErrorAction SilentlyContinue }}
  Remove-Item $target -Force -ErrorAction SilentlyContinue
  if (Test-Path -LiteralPath $backup) {{ Move-Item -LiteralPath $backup -Destination $target -Force }}
  Set-Content -LiteralPath $state -Value 'rolled-back' -Encoding ascii
  Remove-Item $lock -Force -ErrorAction SilentlyContinue
  Start-Process -FilePath $target -ArgumentList @({arguments}) -NoNewWindow
  Remove-Item $PSCommandPath -Force -ErrorAction SilentlyContinue
  exit 1
}}
while (Get-Process -Id $parentPid -ErrorAction SilentlyContinue) {{ Start-Sleep -Seconds 1 }}
Remove-Item $health,$backup -Force -ErrorAction SilentlyContinue
Set-Content -LiteralPath $state -Value 'swapping' -Encoding ascii
try {{
  if (Test-Path -LiteralPath $target) {{ Move-Item -LiteralPath $target -Destination $backup -Force }}
  Move-Item -LiteralPath $candidate -Destination $target -Force
}} catch {{
  if (Test-Path -LiteralPath $backup) {{ Move-Item -LiteralPath $backup -Destination $target -Force }}
  Set-Content -LiteralPath $state -Value 'swap-failed' -Encoding ascii
  Remove-Item $lock -Force -ErrorAction SilentlyContinue
  exit 1
}}
$child = $null
try {{
  $env:{health_env} = $health
  $child = Start-Process -FilePath $target -ArgumentList @({arguments}) -PassThru -NoNewWindow
  Remove-Item Env:{health_env} -ErrorAction SilentlyContinue
  for ($i = 0; $i -lt 100; $i++) {{
    if (Test-Path -LiteralPath $health) {{
      if ($sequenceFile) {{
        Set-Content -LiteralPath "$sequenceFile.tmp" -Value $sequenceValue -Encoding ascii
        Move-Item -LiteralPath "$sequenceFile.tmp" -Destination $sequenceFile -Force
      }}
      Set-Content -LiteralPath $state -Value 'healthy' -Encoding ascii
      Remove-Item $backup,$lock,$PSCommandPath -Force -ErrorAction SilentlyContinue
      exit 0
    }}
    if ($child.HasExited) {{ break }}
    Start-Sleep -Milliseconds 100
    $child.Refresh()
  }}
}} catch {{
  Remove-Item Env:{health_env} -ErrorAction SilentlyContinue
  Restore-Previous $child
}}
Remove-Item Env:{health_env} -ErrorAction SilentlyContinue
Restore-Previous $child
"##,
        backup = powershell_quote_path(backup),
        target = powershell_quote_path(target),
        candidate = powershell_quote_path(candidate),
        state = powershell_quote_path(state),
        health = powershell_quote_path(health),
        lock = powershell_quote_path(lock),
        health_env = HEALTH_FILE_ENV,
    )
}

fn extract_zip(archive: &Path, workspace: &Path) -> MedusaResult<PathBuf> {
    let file = fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file).map_err(zip_error)?;
    let mut candidate = None;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(zip_error)?;
        let enclosed = entry
            .enclosed_name()
            .ok_or_else(|| invalid(format!("unsafe ZIP path {}", entry.name())))?;
        let Some(name) = enclosed.file_name() else {
            continue;
        };
        if !is_medusa_binary(name) || entry.is_dir() {
            continue;
        }
        if candidate.is_some() {
            return Err(invalid("update ZIP contains multiple Medusa executables"));
        }
        let target = workspace.join(name);
        copy_entry(&mut entry, &target)?;
        candidate = Some(target);
    }
    candidate.ok_or_else(|| invalid("update archive does not contain a Medusa executable"))
}

fn extract_tar_gz(archive: &Path, workspace: &Path) -> MedusaResult<PathBuf> {
    let file = fs::File::open(archive)?;
    let mut archive = tar::Archive::new(GzDecoder::new(file));
    let mut candidate = None;
    for entry in archive.entries().map_err(io_error)? {
        let mut entry = entry.map_err(io_error)?;
        let path = entry.path().map_err(io_error)?;
        validate_archive_path(&path)?;
        let Some(name) = path.file_name() else {
            continue;
        };
        if !is_medusa_binary(name) {
            continue;
        }
        if !entry.header().entry_type().is_file() {
            return Err(invalid("Medusa archive entry is not a regular file"));
        }
        if candidate.is_some() {
            return Err(invalid(
                "update archive contains multiple Medusa executables",
            ));
        }
        let target = workspace.join(name);
        copy_entry(&mut entry, &target)?;
        candidate = Some(target);
    }
    candidate.ok_or_else(|| invalid("update archive does not contain a Medusa executable"))
}

fn validate_archive_path(path: &Path) -> MedusaResult<()> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(invalid(format!("unsafe archive path {}", path.display())));
    }
    Ok(())
}

fn copy_entry(reader: &mut impl Read, target: &Path) -> MedusaResult<()> {
    let mut output = fs::File::create(target)?;
    io::copy(reader, &mut output)?;
    output.sync_all()?;
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

fn staged_path(target: &Path) -> PathBuf {
    if cfg!(windows) {
        target.with_extension("update-new.exe")
    } else {
        target.with_extension("update-new")
    }
}

fn backup_path(target: &Path) -> PathBuf {
    if cfg!(windows) {
        target.with_extension("previous.exe")
    } else {
        target.with_extension("previous")
    }
}

fn helper_path(target: &Path) -> PathBuf {
    if cfg!(windows) {
        target.with_extension("update.ps1")
    } else {
        target.with_extension("update.sh")
    }
}

fn shell_quote_path(path: &Path) -> String {
    shell_quote(&path.to_string_lossy())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn powershell_quote_path(path: &Path) -> String {
    powershell_quote(&path.to_string_lossy())
}

fn powershell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> MedusaResult<()> {
    let temporary = path.with_extension("tmp");
    {
        let mut output = fs::File::create(&temporary)?;
        output.write_all(bytes)?;
        output.sync_all()?;
    }
    fs::rename(&temporary, path)?;
    Ok(())
}

fn sync_staged_copy(path: &Path) -> MedusaResult<()> {
    // Windows rejects FlushFileBuffers on a handle opened only for reading
    // (ERROR_ACCESS_DENIED), even though the file itself is writable. Open
    // the staged copy with write access before asking the OS to make it durable.
    OpenOptions::new().write(true).open(path)?.sync_all()?;
    Ok(())
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
    use std::io::Write as _;

    use super::*;

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
    fn concurrent_update_is_refused() {
        let directory = tempfile::tempdir().expect("tempdir");
        let target = directory.path().join(if cfg!(windows) {
            "medusa.exe"
        } else {
            "medusa"
        });
        let candidate = directory.path().join("candidate");
        fs::write(&target, b"old").expect("target");
        fs::write(&candidate, b"new").expect("candidate");
        fs::write(directory.path().join(".medusa-update.lock"), b"locked").expect("lock");
        assert!(
            AtomicInstaller::new(target)
                .schedule_replace(&candidate, &Restart::default(), 1)
                .is_err()
        );
    }

    #[test]
    fn staged_copy_can_be_flushed_before_handoff() {
        let directory = tempfile::tempdir().expect("tempdir");
        let staged = directory.path().join("medusa.update-new.exe");
        fs::write(&staged, b"staged binary").expect("staged binary");

        sync_staged_copy(&staged).expect("flush staged copy");
    }

    #[test]
    fn scripts_require_health_and_contain_rollback() {
        let restart = Restart {
            arguments: vec!["--repo".into(), "repository with spaces".into()],
            sequence_file: Some(PathBuf::from("sequence file")),
            rollout_sequence: Some(42),
        };
        let unix = unix_replace_script(
            42,
            Path::new("/tmp/previous"),
            Path::new("/tmp/medusa"),
            Path::new("/tmp/new"),
            Path::new("/tmp/state"),
            Path::new("/tmp/health"),
            Path::new("/tmp/lock"),
            &restart,
        );
        assert!(unix.contains(HEALTH_FILE_ENV));
        assert!(unix.contains("rolled-back"));
        assert!(unix.contains("repository with spaces"));
        assert!(unix.contains("sequence file"));
        assert!(unix.contains("42"));

        let windows = windows_replace_script(
            42,
            Path::new(r"C:\bin\previous.exe"),
            Path::new(r"C:\bin\medusa.exe"),
            Path::new(r"C:\bin\new.exe"),
            Path::new(r"C:\bin\state"),
            Path::new(r"C:\bin\health"),
            Path::new(r"C:\bin\lock"),
            &restart,
        );
        assert!(windows.contains(HEALTH_FILE_ENV));
        assert!(windows.contains("rolled-back"));
        assert!(windows.contains("Start-Process"));
        assert!(windows.contains("-NoNewWindow"));
        assert!(windows.matches("-NoNewWindow").count() >= 2);
        assert!(windows.contains("Restore-Previous"));
        assert!(windows.contains("sequence file"));
        assert!(windows.contains("42"));
    }

    #[test]
    fn zip_traversal_is_rejected() {
        let directory = tempfile::tempdir().expect("tempdir");
        let archive_path = directory.path().join("malicious.zip");
        let file = fs::File::create(&archive_path).expect("zip");
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file("../medusa", zip::write::SimpleFileOptions::default())
            .expect("entry");
        writer.write_all(b"malicious").expect("body");
        writer.finish().expect("finish");
        assert!(
            AtomicInstaller::new(directory.path().join("target"))
                .extract_archive(&archive_path, &directory.path().join("extract"))
                .is_err()
        );
    }
}
