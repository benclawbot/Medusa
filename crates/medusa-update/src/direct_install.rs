use std::path::{Path, PathBuf};

use medusa_core::MedusaResult;
#[cfg(windows)]
use medusa_core::{ErrorCategory, ErrorCode, MedusaError};

use crate::install::{AtomicInstaller as LegacyAtomicInstaller, Restart, ScheduledUpdate};

/// Platform install facade. Unix keeps the existing health-checked handoff;
/// Windows uses a small external helper that stops Medusa, replaces the exact
/// running executable, verifies the installed bytes, and exits without
/// relaunching the application.
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
        let _ = &self.target;
        #[cfg(windows)]
        {
            retain_legacy_installer_api();
            let _ = (restart, parent_pid);
            schedule_windows_direct_replace(&self.target, candidate)
        }
        #[cfg(not(windows))]
        {
            self.legacy.schedule_replace(candidate, restart, parent_pid)
        }
    }

    pub fn replace(&self, candidate: &Path, restart: &Restart) -> MedusaResult<Option<PathBuf>> {
        #[cfg(windows)]
        {
            let update = self.schedule_replace(candidate, restart, std::process::id())?;
            Ok(Some(update.backup))
        }
        #[cfg(not(windows))]
        {
            self.legacy.replace(candidate, restart)
        }
    }
}

#[cfg(windows)]
fn retain_legacy_installer_api() {
    let _ = (
        LegacyAtomicInstaller::schedule_replace,
        LegacyAtomicInstaller::replace,
    );
}

#[cfg(windows)]
fn schedule_windows_direct_replace(
    target: &Path,
    candidate: &Path,
) -> MedusaResult<ScheduledUpdate> {
    use std::{
        fs::{self, OpenOptions},
        os::windows::process::CommandExt,
        process::{Command, Stdio},
    };

    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

    let metadata = fs::metadata(candidate).map_err(io_error)?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(invalid("update candidate is missing or empty"));
    }

    let directory = target
        .parent()
        .ok_or_else(|| invalid("update target has no parent directory"))?;
    let staged = target.with_extension("update-new.exe");
    let backup = target.with_extension("previous.exe");
    let helper = target.with_extension("update.ps1");
    let state = directory.join(".medusa-update-state");
    let health = directory.join(".medusa-update-health");
    let lock = directory.join(".medusa-update.lock");

    for path in [&staged, &backup, &helper, &lock] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
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
    let label = candidate_version_label(&staged);
    let script = windows_direct_replace_script(
        target,
        &staged,
        &backup,
        &state,
        &health,
        &lock,
        &expected_hash,
        &label,
    );
    fs::write(&helper, script.as_bytes()).map_err(io_error)?;
    fs::write(&lock, b"direct_replace=1\n").map_err(io_error)?;

    Command::new("powershell")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&helper)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .creation_flags(CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .map_err(io_error)?;

    // The running executable cannot replace itself on Windows. Once the helper
    // is live, exit immediately so it can stop the remaining Medusa processes,
    // swap medusa.exe, verify the installed bytes, and emit the only 100% line.
    std::process::exit(0)
}

#[cfg(windows)]
fn candidate_version_label(candidate: &Path) -> String {
    let output = std::process::Command::new(candidate)
        .arg("--version")
        .output();
    let Ok(output) = output else {
        return "new main build".to_owned();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    let Some((_, build)) = text.split_once(" · ") else {
        return text.strip_prefix("medusa ").unwrap_or(text).to_owned();
    };
    match build.strip_prefix("main ") {
        Some(revision) => format!("main ({revision})"),
        None => build.to_owned(),
    }
}

#[cfg(windows)]
fn sha256_file(path: &Path) -> MedusaResult<String> {
    use std::{fs::File, io::Read as _};

    use sha2::{Digest, Sha256};

    let mut file = File::open(path).map_err(io_error)?;
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

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn windows_direct_replace_script(
    target: &Path,
    staged: &Path,
    backup: &Path,
    state: &Path,
    health: &Path,
    lock: &Path,
    expected_hash: &str,
    label: &str,
) -> String {
    format!(
        r##"$ErrorActionPreference = 'Stop'
$target = {target}
$staged = {staged}
$backup = {backup}
$state = {state}
$health = {health}
$lock = {lock}
$expectedHash = {expected_hash}
$label = {label}

function Fail-Update([string]$message) {{
  try {{
    if (-not (Test-Path -LiteralPath $target) -and (Test-Path -LiteralPath $backup)) {{
      Move-Item -LiteralPath $backup -Destination $target -Force
    }}
  }} catch {{}}
  Set-Content -LiteralPath $state -Value "failed: $message" -Encoding ascii
  Remove-Item $lock -Force -ErrorAction SilentlyContinue
  Write-Error "Medusa update failed: $message"
  exit 1
}}

# Stop every Medusa process executing this exact installed medusa.exe. The
# helper is PowerShell, so medusa.exe is released before replacement begins.
Get-Process -Name 'medusa' -ErrorAction SilentlyContinue | ForEach-Object {{
  try {{
    if ($_.Path -eq $target) {{ Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }}
  }} catch {{}}
}}

for ($i = 0; $i -lt 200; $i++) {{
  $running = @(Get-Process -Name 'medusa' -ErrorAction SilentlyContinue | Where-Object {{
    try {{ $_.Path -eq $target }} catch {{ $false }}
  }})
  if ($running.Count -eq 0) {{ break }}
  Start-Sleep -Milliseconds 25
}}

Remove-Item $health -Force -ErrorAction SilentlyContinue
Set-Content -LiteralPath $state -Value 'replacing' -Encoding ascii

$replaced = $false
for ($i = 0; $i -lt 200 -and -not $replaced; $i++) {{
  try {{
    Remove-Item $backup -Force -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $target) {{
      Move-Item -LiteralPath $target -Destination $backup -Force
    }}
    Move-Item -LiteralPath $staged -Destination $target -Force
    $replaced = $true
  }} catch {{
    if (-not (Test-Path -LiteralPath $target) -and (Test-Path -LiteralPath $backup)) {{
      Move-Item -LiteralPath $backup -Destination $target -Force -ErrorAction SilentlyContinue
    }}
    Start-Sleep -Milliseconds 25
  }}
}}
if (-not $replaced) {{ Fail-Update 'could not replace medusa.exe' }}

$actualHash = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actualHash -ne $expectedHash) {{
  Remove-Item $target -Force -ErrorAction SilentlyContinue
  if (Test-Path -LiteralPath $backup) {{ Move-Item -LiteralPath $backup -Destination $target -Force }}
  Fail-Update 'installed executable failed SHA-256 verification'
}}

Set-Content -LiteralPath $state -Value 'updated' -Encoding ascii
Remove-Item $backup,$lock -Force -ErrorAction SilentlyContinue
Write-Host 'Updating Medusa [████████████████████████████████] 100% · Complete'
Write-Host "Updated to: $label"
Remove-Item $PSCommandPath -Force -ErrorAction SilentlyContinue
exit 0
"##,
        target = powershell_quote_path(target),
        staged = powershell_quote_path(staged),
        backup = powershell_quote_path(backup),
        state = powershell_quote_path(state),
        health = powershell_quote_path(health),
        lock = powershell_quote_path(lock),
        expected_hash = powershell_quote(expected_hash),
        label = powershell_quote(label),
    )
}

#[cfg(windows)]
fn powershell_quote_path(path: &Path) -> String {
    powershell_quote(&path.to_string_lossy())
}

#[cfg(windows)]
fn powershell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(windows)]
fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

#[cfg(windows)]
fn io_error(error: impl std::fmt::Display) -> MedusaError {
    MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Execution,
        error.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facade_keeps_target_for_platform_specific_install() {
        let target = PathBuf::from("medusa-test-target");
        let installer = AtomicInstaller::new(target.clone());
        assert_eq!(installer.target, target);
    }

    #[cfg(windows)]
    #[test]
    fn windows_helper_stops_medusa_before_replacing_and_reports_completion_last() {
        let script = windows_direct_replace_script(
            Path::new(r"C:\bin\medusa.exe"),
            Path::new(r"C:\bin\medusa.update-new.exe"),
            Path::new(r"C:\bin\medusa.previous.exe"),
            Path::new(r"C:\bin\.medusa-update-state"),
            Path::new(r"C:\bin\.medusa-update-health"),
            Path::new(r"C:\bin\.medusa-update.lock"),
            "abc123",
            "main (deadbeef1234)",
        );
        let stop = script.find("Stop-Process").expect("stop processes");
        let replace = script
            .find("Move-Item -LiteralPath $staged")
            .expect("replace executable");
        let complete = script.find("100% · Complete").expect("completion output");
        assert!(stop < replace);
        assert!(replace < complete);
        assert!(script.contains("Updated to: $label"));
        assert!(!script.contains("Start-UpdatedProcess"));
    }
}
