#!/usr/bin/env python3
"""Make rollout sequence commitment and startup rollback health-atomic."""

from __future__ import annotations

from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise RuntimeError(f"{label}: expected one match, found {count}")
    return text.replace(old, new, 1)


def update_install() -> None:
    path = Path("crates/medusa-update/src/install.rs")
    text = path.read_text(encoding="utf-8")
    text = replace_once(
        text,
        '''pub struct Restart {
    pub arguments: Vec<String>,
}''',
        '''pub struct Restart {
    pub arguments: Vec<String>,
    /// Commit this rollout sequence only after the replacement acknowledges startup.
    pub sequence_file: Option<PathBuf>,
    pub rollout_sequence: Option<u64>,
}''',
        "restart rollout fields",
    )
    text = replace_once(
        text,
        '''        validate_candidate(candidate)?;
        self.recover_interrupted()?;''',
        '''        validate_candidate(candidate)?;
        if restart.sequence_file.is_some() != restart.rollout_sequence.is_some() {
            return Err(invalid(
                "restart sequence file and rollout sequence must be configured together",
            ));
        }
        if restart.rollout_sequence == Some(0) {
            return Err(invalid("rollout sequence must be positive"));
        }
        self.recover_interrupted()?;''',
        "restart rollout validation",
    )

    unix_start = text.index("fn unix_replace_script(")
    windows_start = text.index("\nfn windows_replace_script(", unix_start)
    extract_start = text.index("\nfn extract_zip(", windows_start)
    unix_and_windows = r'''fn unix_replace_script(
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
  Start-Process -FilePath $target -ArgumentList @({arguments})
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
  $child = Start-Process -FilePath $target -ArgumentList @({arguments}) -PassThru
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
}'''
    text = text[:unix_start] + unix_and_windows + text[extract_start:]
    text = replace_once(
        text,
        '''        let restart = Restart {
            arguments: vec!["--repo".into(), "repository with spaces".into()],
        };''',
        '''        let restart = Restart {
            arguments: vec!["--repo".into(), "repository with spaces".into()],
            sequence_file: Some(PathBuf::from("sequence file")),
            rollout_sequence: Some(42),
        };''',
        "script fixture restart sequence",
    )
    text = replace_once(
        text,
        '''        assert!(unix.contains("repository with spaces"));

        let windows''',
        '''        assert!(unix.contains("repository with spaces"));
        assert!(unix.contains("sequence file"));
        assert!(unix.contains("42"));

        let windows''',
        "Unix sequence script assertions",
    )
    text = replace_once(
        text,
        '''        assert!(windows.contains("Start-Process"));
    }''',
        '''        assert!(windows.contains("Start-Process"));
        assert!(windows.contains("Restore-Previous"));
        assert!(windows.contains("sequence file"));
        assert!(windows.contains("42"));
    }''',
        "Windows sequence script assertions",
    )
    path.write_text(text, encoding="utf-8")
