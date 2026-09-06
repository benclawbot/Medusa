use std::path::Path;

use crate::install::{HEALTH_FILE_ENV, HEALTH_NONCE_ENV, Restart};

#[derive(Clone, Copy)]
pub(super) struct WindowsReplaceScript<'a> {
    pub parent_pid: u32,
    pub target: &'a Path,
    pub staged: &'a Path,
    pub backup: &'a Path,
    pub state: &'a Path,
    pub health: &'a Path,
    pub outcome: &'a Path,
    pub health_nonce: &'a str,
    pub target_revision: Option<&'a str>,
    pub lock: &'a Path,
    pub expected_hash: &'a str,
    pub restart: &'a Restart,
}

pub(super) fn windows_health_checked_replace_script(spec: WindowsReplaceScript<'_>) -> String {
    let WindowsReplaceScript {
        parent_pid,
        target,
        staged,
        backup,
        state,
        health,
        outcome,
        health_nonce,
        target_revision,
        lock,
        expected_hash,
        restart,
    } = spec;
    let arguments = windows_process_arguments(&restart.arguments);
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
$target = {target}
$staged = {staged}
$backup = {backup}
$state = {state}
$health = {health}
$outcome = {outcome}
$healthNonce = {health_nonce}
$targetRevision = {target_revision}
$lock = {lock}
$expectedHash = {expected_hash}
$restartArguments = {arguments}
$sequenceFile = {sequence_file}
$sequenceValue = {sequence_value}

function Start-Medusa([bool]$WithHealth) {{
  $start = New-Object System.Diagnostics.ProcessStartInfo
  $start.FileName = $target
  $start.Arguments = $restartArguments
  $start.UseShellExecute = $false
  $start.WorkingDirectory = [System.IO.Path]::GetDirectoryName($target)
  $start.CreateNoWindow = {create_no_window}
  if ($WithHealth) {{
    $start.EnvironmentVariables[{health_env}] = $health
    $start.EnvironmentVariables[{health_nonce_env}] = $healthNonce
  }} else {{
    [void]$start.EnvironmentVariables.Remove({health_env})
    [void]$start.EnvironmentVariables.Remove({health_nonce_env})
  }}
  [System.Diagnostics.Process]::Start($start)
}}

function Restore-Previous([string]$Reason, $Child) {{
  try {{
    if ($null -ne $Child -and -not $Child.HasExited) {{
      $Child.Kill()
      $Child.WaitForExit(5000) | Out-Null
    }}
  }} catch {{}}
  Remove-Item $target -Force -ErrorAction SilentlyContinue
  if (Test-Path -LiteralPath $backup) {{
    Move-Item -LiteralPath $backup -Destination $target -Force
  }}
  Set-Content -LiteralPath $state -Value 'rolled-back' -Encoding ascii
  Write-Outcome 'rolled-back' $Reason 'restored'
  Remove-Item $lock -Force -ErrorAction SilentlyContinue
  try {{ Start-Medusa $false | Out-Null }} catch {{}}
  Write-Error "Medusa update failed: $Reason"
  Remove-Item $PSCommandPath -Force -ErrorAction SilentlyContinue
  exit 1
}}

function Write-Outcome([string]$Stage, [string]$Reason, [string]$RollbackResult) {{
  $record = @{{
    schema = 1
    targetRevision = if ($targetRevision -eq '') {{ $null }} else {{ $targetRevision }}
    previousRevision = $null
    stage = $Stage
    reason = $Reason
    startedUnixSeconds = 0
    finishedUnixSeconds = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    rollbackResult = $RollbackResult
  }} | ConvertTo-Json -Compress
  $temporary = "$outcome.tmp.$PID"
  Set-Content -LiteralPath $temporary -Value $record -Encoding utf8
  Move-Item -LiteralPath $temporary -Destination $outcome -Force
}}

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
  Remove-Item $PSCommandPath -Force -ErrorAction SilentlyContinue
  exit 1
}}

$targetFull = [System.IO.Path]::GetFullPath($target)
function Get-TargetProcesses {{
  @(
    Get-Process -ErrorAction SilentlyContinue | Where-Object {{
      if ($parentPid -ne 0 -and $_.Id -eq $parentPid) {{
        $true
      }} else {{
        try {{
          [string]::Equals(
            [System.IO.Path]::GetFullPath($_.Path),
            $targetFull,
            [System.StringComparison]::OrdinalIgnoreCase
          )
        }} catch {{
          $false
        }}
      }}
    }}
  )
}}

Get-TargetProcesses | Stop-Process -Force -ErrorAction SilentlyContinue
for ($i = 0; $i -lt 100; $i++) {{
  if ((Get-TargetProcesses).Count -eq 0) {{ break }}
  Start-Sleep -Milliseconds 100
}}
if ((Get-TargetProcesses).Count -ne 0) {{
  Set-Content -LiteralPath $state -Value 'stop-failed' -Encoding ascii
  Remove-Item $lock -Force -ErrorAction SilentlyContinue
  Write-Error 'Medusa update failed: could not stop all processes using the target executable'
  Remove-Item $PSCommandPath -Force -ErrorAction SilentlyContinue
  exit 1
}}

Remove-Item $health,$backup -Force -ErrorAction SilentlyContinue
Set-Content -LiteralPath $state -Value 'swapping' -Encoding ascii
try {{
  if (Test-Path -LiteralPath $target) {{
    Move-Item -LiteralPath $target -Destination $backup -Force
  }}
  Move-Item -LiteralPath $staged -Destination $target -Force
}} catch {{
  if (Test-Path -LiteralPath $backup) {{
    Move-Item -LiteralPath $backup -Destination $target -Force
  }}
  Set-Content -LiteralPath $state -Value 'swap-failed' -Encoding ascii
  Remove-Item $lock -Force -ErrorAction SilentlyContinue
  Write-Error "Medusa update failed while replacing the executable: $($_.Exception.Message)"
  Remove-Item $PSCommandPath -Force -ErrorAction SilentlyContinue
  exit 1
}}

try {{
  $sha256 = [System.Security.Cryptography.SHA256]::Create()
  try {{
    $stream = [System.IO.File]::OpenRead($target)
    try {{
      $hashBytes = $sha256.ComputeHash($stream)
    }} finally {{
      $stream.Dispose()
    }}
  }} finally {{
    $sha256.Dispose()
  }}
  $actualHash = ([System.BitConverter]::ToString($hashBytes) -replace '-', '').ToLowerInvariant()
}} catch {{
  Restore-Previous ("installed executable SHA-256 verification failed: " + $_.Exception.Message) $null
}}
if ($actualHash -ne $expectedHash) {{
  Restore-Previous 'installed executable failed SHA-256 verification' $null
}}

$child = $null
try {{
  $child = Start-Medusa $true
}} catch {{
  Restore-Previous ("replacement executable could not start: " + $_.Exception.Message) $null
}}

for ($i = 0; $i -lt 600; $i++) {{
  if (Test-Path -LiteralPath $health) {{
    $healthText = Get-Content -LiteralPath $health -Raw -ErrorAction SilentlyContinue
    if ($healthText -match ('"nonce"\s*:\s*"' + [regex]::Escape($healthNonce) + '"')) {{
      if ($sequenceFile) {{
        Set-Content -LiteralPath "$sequenceFile.tmp" -Value $sequenceValue -Encoding ascii
        Move-Item -LiteralPath "$sequenceFile.tmp" -Destination $sequenceFile -Force
      }}
      Set-Content -LiteralPath $state -Value 'healthy' -Encoding ascii
      Write-Outcome 'healthy' 'replacement acknowledged startup health' 'not-required'
      Remove-Item $backup,$lock -Force -ErrorAction SilentlyContinue
      Write-Host 'Updating Medusa [████████████████████████████████] 100% · Complete'
      Remove-Item $PSCommandPath -Force -ErrorAction SilentlyContinue
      exit 0
    }}
  }}
  if ($child.HasExited) {{
    Restore-Previous ("replacement exited before health acknowledgement (exit " + $child.ExitCode + ")") $child
  }}
  Start-Sleep -Milliseconds 100
}}

Restore-Previous 'replacement did not acknowledge startup health before timeout' $child
"##,
        target = powershell_quote_path(target),
        staged = powershell_quote_path(staged),
        backup = powershell_quote_path(backup),
        state = powershell_quote_path(state),
        health = powershell_quote_path(health),
        outcome = powershell_quote_path(outcome),
        health_nonce = powershell_quote(health_nonce),
        target_revision = powershell_quote(target_revision.unwrap_or_default()),
        lock = powershell_quote_path(lock),
        create_no_window = if restart.detached { "$true" } else { "$false" },
        expected_hash = powershell_quote(expected_hash),
        arguments = powershell_quote(&arguments),
        sequence_file = sequence_file,
        sequence_value = sequence_value,
        health_env = powershell_quote(HEALTH_FILE_ENV),
        health_nonce_env = powershell_quote(HEALTH_NONCE_ENV),
    )
}

fn windows_process_arguments(arguments: &[String]) -> String {
    arguments
        .iter()
        .map(|argument| windows_quote_argument(argument))
        .collect::<Vec<_>>()
        .join(" ")
}

fn windows_quote_argument(argument: &str) -> String {
    if argument.is_empty() {
        return "\"\"".to_owned();
    }
    if !argument
        .bytes()
        .any(|byte| byte.is_ascii_whitespace() || byte == b'"')
    {
        return argument.to_owned();
    }

    let mut quoted = String::from("\"");
    let mut backslashes = 0_usize;
    for character in argument.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                quoted.push_str(&"\\".repeat(backslashes.saturating_mul(2).saturating_add(1)));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                quoted.push(character);
            }
        }
    }
    quoted.push_str(&"\\".repeat(backslashes.saturating_mul(2)));
    quoted.push('"');
    quoted
}

fn powershell_quote_path(path: &Path) -> String {
    powershell_quote(&path.to_string_lossy())
}

fn powershell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_restart_arguments_for_windows_process_creation() {
        assert_eq!(windows_quote_argument("--fresh"), "--fresh");
        assert_eq!(
            windows_quote_argument(r"C:\repo with spaces"),
            r#""C:\repo with spaces""#
        );
        assert_eq!(
            windows_quote_argument(r#"value "quoted" here"#),
            r#""value \"quoted\" here""#
        );
        assert_eq!(
            windows_quote_argument(r"C:\path with slash\"),
            r#""C:\path with slash\\""#
        );
    }
}
