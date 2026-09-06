use std::{
    env, fs,
    fs::OpenOptions,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use flate2::read::GzDecoder;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult, storage};
use medusa_process_containment::process_start_marker;

use crate::model::invalid;

pub const HEALTH_FILE_ENV: &str = "MEDUSA_UPDATE_HEALTH_FILE";
pub const HEALTH_NONCE_ENV: &str = "MEDUSA_UPDATE_HEALTH_NONCE";
pub const UPDATE_OUTCOME_FILE: &str = ".medusa-update-outcome.json";
const HEALTH_CHECK_ATTEMPTS: usize = 600;
const UPDATE_LOCK_SCHEMA: &str = "2";

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
    /// Launch the replacement as an independent desktop process rather than sharing the
    /// updater helper's console.
    pub detached: bool,
    /// Commit this rollout sequence only after the replacement acknowledges startup.
    pub sequence_file: Option<PathBuf>,
    pub rollout_sequence: Option<u64>,
    /// Immutable source identity for the replacement, when known.
    pub target_revision: Option<String>,
}

/// Paths retained by the detached replacement helper.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduledUpdate {
    pub helper: PathBuf,
    pub backup: PathBuf,
    pub state: PathBuf,
    pub health: PathBuf,
    pub outcome: PathBuf,
    pub health_nonce: String,
}

/// Redacted durable result of the last attempted replacement.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateOutcome {
    pub schema: u8,
    pub target_revision: Option<String>,
    pub previous_revision: Option<String>,
    pub stage: String,
    pub reason: String,
    pub started_unix_seconds: i64,
    pub finished_unix_seconds: i64,
    pub rollback_result: String,
}

pub fn read_update_outcome(directory: &Path) -> MedusaResult<Option<UpdateOutcome>> {
    let path = directory.join(UPDATE_OUTCOME_FILE);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| invalid(format!("invalid update outcome: {error}"))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(error)),
    }
}

/// Acknowledges that a newly started binary completed its startup path.
pub fn acknowledge_update_health() -> MedusaResult<bool> {
    acknowledge_update_health_values(
        env::var_os(HEALTH_FILE_ENV).map(PathBuf::from),
        env::var_os(HEALTH_NONCE_ENV).map(|value| value.to_string_lossy().into_owned()),
    )
}

fn acknowledge_update_health_values(
    path: Option<PathBuf>,
    nonce: Option<String>,
) -> MedusaResult<bool> {
    let Some(path) = path else {
        return Ok(false);
    };
    if let Some(nonce) = nonce.as_deref()
        && (nonce.is_empty()
            || nonce.len() > 128
            || nonce.bytes().any(|byte| !byte.is_ascii_hexdigit()))
    {
        return Err(invalid("update health nonce is malformed"));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let Some(nonce) = nonce else {
        // Older helpers only require a non-empty marker. Keep an in-flight
        // update created by an older binary compatible with this binary.
        storage::atomic_write(&path, b"healthy\n")?;
        return Ok(true);
    };
    let payload = serde_json::json!({
        "schema": 1,
        "nonce": nonce,
        "stage": "startup-ready",
        "recordedUnixSeconds": unix_timestamp(),
    });
    let bytes = serde_json::to_vec(&payload).map_err(|error| invalid(error.to_string()))?;
    storage::atomic_write(&path, &bytes)?;
    Ok(true)
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().try_into().unwrap_or(i64::MAX))
        .unwrap_or_default()
}

pub(crate) fn new_health_nonce(_target: &Path, _parent_pid: u32) -> MedusaResult<String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| {
        io_error(io::Error::other(format!(
            "failed to generate update nonce: {error}"
        )))
    })?;
    Ok(hex::encode(bytes))
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

    /// Stages the candidate beside the running executable and starts a replacement helper.
    /// On Windows the helper owns the final handoff: it stops processes using this exact
    /// installation, replaces the executable, verifies the new binary, and reports success.
    pub fn schedule_replace(
        &self,
        candidate: &Path,
        restart: &Restart,
        parent_pid: u32,
    ) -> MedusaResult<ScheduledUpdate> {
        self.schedule_replace_with_revision(candidate, restart, parent_pid, None)
    }

    pub fn schedule_replace_with_revision(
        &self,
        candidate: &Path,
        restart: &Restart,
        parent_pid: u32,
        target_revision: Option<&str>,
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
        let directory = self
            .target
            .parent()
            .ok_or_else(|| invalid("update target has no parent directory"))?;
        let lock = directory.join(".medusa-update.lock");
        let mut lock_file = acquire_update_lock(&lock, parent_pid)?;

        let staged = staged_path(&self.target);
        let backup = backup_path(&self.target);
        let state = directory.join(".medusa-update-state");
        let health = directory.join(".medusa-update-health");
        let outcome = directory.join(UPDATE_OUTCOME_FILE);
        let helper = helper_path(&self.target);
        let nonce = new_health_nonce(&self.target, parent_pid)?;
        let target_revision = target_revision.or(restart.target_revision.as_deref());
        if let Some(revision) = target_revision {
            validate_target_revision(revision)?;
        }
        let mut helper_process = None;
        let result = (|| -> MedusaResult<()> {
            self.recover_interrupted()?;
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
                    &outcome,
                    &nonce,
                    target_revision,
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
                    &outcome,
                    &nonce,
                    target_revision,
                    &lock,
                    restart,
                )
            };
            storage::atomic_write(&helper, script.as_bytes())?;
            #[cfg(unix)]
            set_executable(&helper)?;
            let child = helper_command(&helper).spawn().map_err(io_error)?;
            let child_pid = child.id();
            helper_process = Some(child);
            writeln!(lock_file, "helper_pid={child_pid}")?;
            if let Some(identity) = process_identity(child_pid)? {
                writeln!(lock_file, "helper_identity={identity}")?;
            }
            writeln!(lock_file, "helper_ready=1")?;
            lock_file.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            if let Some(mut child) = helper_process {
                let _ = child.kill();
                let _ = child.wait();
            }
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
            outcome,
            health_nonce: nonce,
        })
    }

    /// Compatibility wrapper. Replacement is always delayed and health checked.
    pub fn replace(&self, candidate: &Path, restart: &Restart) -> MedusaResult<Option<PathBuf>> {
        let update = self.schedule_replace(candidate, restart, std::process::id())?;
        Ok(Some(update.backup))
    }
}

fn acquire_update_lock(lock: &Path, parent_pid: u32) -> MedusaResult<fs::File> {
    loop {
        match OpenOptions::new().write(true).create_new(true).open(lock) {
            Ok(mut file) => {
                let result = (|| -> MedusaResult<()> {
                    writeln!(file, "schema={UPDATE_LOCK_SCHEMA}")?;
                    writeln!(file, "parent_pid={parent_pid}")?;
                    if let Some(identity) = process_identity(parent_pid)? {
                        writeln!(file, "parent_identity={identity}")?;
                    }
                    writeln!(file, "lock_ready=1")?;
                    file.sync_all()?;
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
    let Some(parent_pid) = lock_field(&content, "parent_pid").and_then(|value| value.parse().ok())
    else {
        return Ok(true);
    };
    let schema = lock_field(&content, "schema");
    if schema.is_some_and(|value| value != UPDATE_LOCK_SCHEMA) {
        return Ok(true);
    }
    if schema.is_some() && lock_field(&content, "lock_ready") != Some("1") {
        if process_matches(parent_pid, lock_field(&content, "parent_identity"))? {
            return Ok(true);
        }
        return Ok(true);
    }
    if process_matches(parent_pid, lock_field(&content, "parent_identity"))? {
        return Ok(true);
    }
    if let Some(helper_value) = lock_field(&content, "helper_pid") {
        let Ok(helper_pid) = helper_value.parse() else {
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

fn helper_command(script: &Path) -> Command {
    if cfg!(windows) {
        #[cfg(windows)]
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
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
        // Keep the helper in the caller's console so it can report 100% only after
        // the executable has actually been replaced. It is still a separate process,
        // which lets it terminate the running Medusa binary before moving the new one.
        #[cfg(windows)]
        command.creation_flags(CREATE_NEW_PROCESS_GROUP);
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
    outcome: &Path,
    health_nonce: &str,
    target_revision: Option<&str>,
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
outcome={outcome}
health_nonce={health_nonce}
target_revision={target_revision}
lock={lock}
sequence_file={sequence_file}
sequence_value={sequence_value}
child=''
write_outcome() {{
  stage=$1
  reason=$2
  rollback_result=$3
  finished=$(date +%s)
  tmp="$outcome.tmp.$$"
  printf '{{"schema":1,"targetRevision":%s,"previousRevision":null,"stage":"%s","reason":"%s","startedUnixSeconds":%s,"finishedUnixSeconds":%s,"rollbackResult":"%s"}}\n' \
    "$target_revision" "$stage" "$reason" "$started" "$finished" "$rollback_result" > "$tmp" &&
    mv -f "$tmp" "$outcome"
}}
rollback() {{
  if [ -n "$child" ]; then kill "$child" 2>/dev/null || true; fi
  rm -f "$target"
  if [ -e "$backup" ]; then mv "$backup" "$target"; fi
  printf 'rolled-back\n' > "$state"
  write_outcome 'rolled-back' 'replacement failed' 'restored'
  rm -f "$lock"
  "$target" {arguments} >/dev/null 2>&1 &
  rm -f "$0"
  exit 1
}}
started=$(date +%s)
while kill -0 "$parent" 2>/dev/null; do sleep 1; done
rm -f "$health" "$backup"
printf 'swapping\n' > "$state"
if [ -e "$target" ]; then mv "$target" "$backup"; fi
if ! mv "$candidate" "$target"; then
  if [ -e "$backup" ]; then mv "$backup" "$target"; fi
  printf 'swap-failed\n' > "$state"
  write_outcome 'swap-failed' 'replacement failed while moving candidate' 'restored'
  rm -f "$lock"
  if [ -x "$target" ]; then "$target" {arguments} >/dev/null 2>&1 & fi
  rm -f "$0"
  exit 1
fi
chmod 755 "$target" || rollback
cd "$(dirname "$target")"
MEDUSA_UPDATE_HEALTH_FILE="$health" MEDUSA_UPDATE_HEALTH_NONCE="$health_nonce" "$target" {arguments} &
child=$!
i=0
while [ "$i" -lt {health_check_attempts} ]; do
  if [ -s "$health" ]; then
    if ! grep -Fq '"nonce":"'"$health_nonce"'"' "$health"; then
      i=$((i + 1)); sleep 0.1; continue
    fi
    if [ -n "$sequence_file" ]; then
      printf '%s\n' "$sequence_value" > "$sequence_file.tmp" || rollback
      mv "$sequence_file.tmp" "$sequence_file" || rollback
    fi
    printf 'healthy\n' > "$state"
    write_outcome 'healthy' 'replacement acknowledged startup health' 'not-required'
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
        outcome = shell_quote_path(outcome),
        health_nonce = shell_quote(health_nonce),
        target_revision = shell_quote(
            &target_revision
                .map(|revision| format!("\"{revision}\""))
                .unwrap_or_else(|| "null".to_owned()),
        ),
        lock = shell_quote_path(lock),
        health_check_attempts = HEALTH_CHECK_ATTEMPTS,
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
    outcome: &Path,
    health_nonce: &str,
    target_revision: Option<&str>,
    lock: &Path,
    restart: &Restart,
) -> String {
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
$outcome = {outcome}
$healthNonce = {health_nonce}
$targetRevision = {target_revision}
$lock = {lock}
$sequenceFile = {sequence_file}
$sequenceValue = {sequence_value}

function Restore-Previous([string]$Reason) {{
  Remove-Item $target -Force -ErrorAction SilentlyContinue
  if (Test-Path -LiteralPath $backup) {{ Move-Item -LiteralPath $backup -Destination $target -Force }}
  Set-Content -LiteralPath $state -Value 'rolled-back' -Encoding ascii
  Remove-Item $lock -Force -ErrorAction SilentlyContinue
  Write-Error "Medusa update failed: $Reason"
  Remove-Item $PSCommandPath -Force -ErrorAction SilentlyContinue
  exit 1
}}

# schedule_replace writes helper_ready only after the helper process has started.
# Waiting for that marker prevents us from killing the parent before staging is durable.
$helperReady = $false
for ($i = 0; $i -lt 200; $i++) {{
  if (Test-Path -LiteralPath $lock) {{
    $helperReady = Select-String -LiteralPath $lock -Pattern '^helper_ready=1$' -Quiet -ErrorAction SilentlyContinue
    if ($helperReady) {{ break }}
  }}
  Start-Sleep -Milliseconds 10
}}
if (-not $helperReady) {{
  Set-Content -LiteralPath $state -Value 'helper-not-ready' -Encoding ascii
  Remove-Item $lock -Force -ErrorAction SilentlyContinue
  exit 1
}}
Start-Sleep -Milliseconds 100

$targetFull = [System.IO.Path]::GetFullPath($target)
function Get-TargetMedusaProcesses {{
  @(
    Get-Process -Name 'medusa' -ErrorAction SilentlyContinue | Where-Object {{
      try {{
        [string]::Equals(
          [System.IO.Path]::GetFullPath($_.Path),
          $targetFull,
          [System.StringComparison]::OrdinalIgnoreCase
        )
      }} catch {{
        $_.Id -eq $parentPid
      }}
    }}
  )
}}

# Stop every Medusa process using this exact installation, including the updater itself.
Get-TargetMedusaProcesses | Stop-Process -Force -ErrorAction SilentlyContinue
for ($i = 0; $i -lt 100; $i++) {{
  if ((Get-TargetMedusaProcesses).Count -eq 0) {{ break }}
  Start-Sleep -Milliseconds 100
}}
if ((Get-TargetMedusaProcesses).Count -ne 0) {{
  Set-Content -LiteralPath $state -Value 'stop-failed' -Encoding ascii
  Remove-Item $lock -Force -ErrorAction SilentlyContinue
  Write-Error 'Medusa update failed: could not stop all processes using medusa.exe'
  exit 1
}}

Remove-Item $health,$backup -Force -ErrorAction SilentlyContinue
Set-Content -LiteralPath $state -Value 'swapping' -Encoding ascii
try {{
  if (Test-Path -LiteralPath $target) {{ Move-Item -LiteralPath $target -Destination $backup -Force }}
  Move-Item -LiteralPath $candidate -Destination $target -Force
}} catch {{
  if (Test-Path -LiteralPath $backup) {{ Move-Item -LiteralPath $backup -Destination $target -Force }}
  Set-Content -LiteralPath $state -Value 'swap-failed' -Encoding ascii
  Remove-Item $lock -Force -ErrorAction SilentlyContinue
  Write-Error "Medusa update failed while replacing medusa.exe: $($_.Exception.Message)"
  exit 1
}}

try {{
  $versionOutput = (& $target --version 2>&1 | Out-String).Trim()
  if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($versionOutput)) {{
    throw 'replacement executable did not pass --version verification'
  }}
  if ($sequenceFile) {{
    Set-Content -LiteralPath "$sequenceFile.tmp" -Value $sequenceValue -Encoding ascii
    Move-Item -LiteralPath "$sequenceFile.tmp" -Destination $sequenceFile -Force
  }}
  Set-Content -LiteralPath $state -Value 'updated' -Encoding ascii
  Remove-Item $backup,$lock -Force -ErrorAction SilentlyContinue

  Write-Host ''
  Write-Host 'Updating Medusa [████████████████████████████████] 100% · Complete'
  if ($versionOutput -match 'main\s+([0-9a-fA-F]{{12}})') {{
    Write-Host ("Updated to: main (" + $Matches[1].ToLowerInvariant() + ")")
  }} else {{
    $displayVersion = $versionOutput -replace '^medusa\s+', ''
    Write-Host ("Updated to: " + $displayVersion)
  }}
  Remove-Item $PSCommandPath -Force -ErrorAction SilentlyContinue
  exit 0
}} catch {{
  Restore-Previous $_.Exception.Message
}}
"##,
        backup = powershell_quote_path(backup),
        target = powershell_quote_path(target),
        candidate = powershell_quote_path(candidate),
        state = powershell_quote_path(state),
        health = powershell_quote_path(health),
        outcome = powershell_quote_path(outcome),
        health_nonce = powershell_quote(health_nonce),
        target_revision = powershell_quote(target_revision.unwrap_or_default()),
        lock = powershell_quote_path(lock),
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

fn validate_target_revision(revision: &str) -> MedusaResult<()> {
    if revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(invalid(
            "update target revision must be a full 40-character Git commit SHA",
        ))
    }
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
    fn stale_update_lock_is_reclaimed() {
        let directory = tempfile::tempdir().expect("tempdir");
        let lock = directory.path().join(".medusa-update.lock");
        fs::write(&lock, "parent_pid=4294967295\n").expect("stale lock");
        assert!(!update_owner_is_alive(&lock).expect("inspect stale lock"));

        let new_lock = acquire_update_lock(&lock, std::process::id()).expect("reclaim lock");
        drop(new_lock);
        let contents = fs::read_to_string(&lock).expect("read lock");
        assert!(contents.contains("schema=2"));
        assert!(contents.contains("lock_ready=1"));
    }

    #[test]
    fn active_update_lock_is_preserved() {
        let directory = tempfile::tempdir().expect("tempdir");
        let lock = directory.path().join(".medusa-update.lock");
        let identity = process_identity(std::process::id())
            .expect("process identity")
            .expect("current process identity");
        fs::write(
            &lock,
            format!(
                "schema=2\nparent_pid={}\nparent_identity={identity}\nlock_ready=1\n",
                std::process::id()
            ),
        )
        .expect("active lock");

        assert!(update_owner_is_alive(&lock).expect("inspect lock"));
    }

    #[test]
    fn staged_copy_can_be_flushed_before_handoff() {
        let directory = tempfile::tempdir().expect("tempdir");
        let staged = directory.path().join("medusa.update-new.exe");
        fs::write(&staged, b"staged binary").expect("staged binary");

        sync_staged_copy(&staged).expect("flush staged copy");
    }

    #[test]
    fn scripts_keep_unix_health_checks_and_windows_direct_replacement() {
        let restart = Restart {
            arguments: vec!["--repo".into(), "repository with spaces".into()],
            detached: false,
            sequence_file: Some(PathBuf::from("sequence file")),
            rollout_sequence: Some(42),
            target_revision: None,
        };
        let unix = unix_replace_script(
            42,
            Path::new("/tmp/previous"),
            Path::new("/tmp/medusa"),
            Path::new("/tmp/new"),
            Path::new("/tmp/state"),
            Path::new("/tmp/health"),
            Path::new("/tmp/outcome"),
            "0123456789abcdef0123456789abcdef",
            None,
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
            Path::new(r"C:\bin\outcome"),
            "0123456789abcdef0123456789abcdef",
            None,
            Path::new(r"C:\bin\lock"),
            &restart,
        );
        assert!(windows.contains("Get-TargetMedusaProcesses"));
        assert!(windows.contains("Stop-Process -Force"));
        assert!(windows.contains("Move-Item -LiteralPath $candidate -Destination $target -Force"));
        assert!(windows.contains("$target --version"));
        assert!(windows.contains("100% · Complete"));
        assert!(windows.contains("Updated to: main ("));
        assert!(windows.contains("sequence file"));
        assert!(windows.contains("42"));
        assert!(!windows.contains("Start-UpdatedProcess"));
        assert!(!windows.contains("-NoNewWindow"));
    }

    #[test]
    #[serial_test::serial]
    fn health_acknowledgement_is_nonce_bound_and_keeps_legacy_helpers_compatible() {
        let directory = tempfile::tempdir().expect("tempdir");
        let health = directory.path().join("health.json");
        let nonce = "0123456789abcdef0123456789abcdef".to_owned();
        assert!(
            acknowledge_update_health_values(Some(health.clone()), Some(nonce.clone()))
                .expect("nonce health acknowledgement")
        );
        let payload: serde_json::Value =
            serde_json::from_slice(&fs::read(&health).expect("health payload"))
                .expect("valid health payload");
        assert_eq!(payload["nonce"], nonce);
        assert_eq!(payload["stage"], "startup-ready");

        assert!(
            acknowledge_update_health_values(Some(health.clone()), None)
                .expect("legacy health acknowledgement")
        );
        assert_eq!(
            fs::read_to_string(&health).expect("legacy health marker"),
            "healthy\n"
        );
    }

    #[test]
    #[serial_test::serial]
    fn malformed_health_nonce_is_rejected_before_writing() {
        let directory = tempfile::tempdir().expect("tempdir");
        let health = directory.path().join("health.json");
        assert!(
            acknowledge_update_health_values(
                Some(health.clone()),
                Some("not-a-hex-nonce".to_owned())
            )
            .is_err()
        );
        assert!(!health.exists());
    }

    #[test]
    fn generated_health_nonces_are_unpredictable_and_well_formed() {
        let first = new_health_nonce(Path::new("medusa"), 42).expect("first nonce");
        let second = new_health_nonce(Path::new("medusa"), 42).expect("second nonce");
        assert_eq!(first.len(), 32);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first, second);
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
