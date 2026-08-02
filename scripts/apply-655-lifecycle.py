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
        .unwrap_or_else(|| "''".to_owned());
    let sequence_value = restart
        .rollout_sequence
        .map(|value| shell_quote(&value.to_string()))
        .unwrap_or_else(|| "''".to_owned());
    format!(
        "#!/bin/sh\nset -eu\nparent={parent_pid}\nbackup={backup}\ntarget={target}\ncandidate={candidate}\nstate={state}\nhealth={health}\nlock={lock}\nsequence_file={sequence_file}\nsequence_value={sequence_value}\nchild=''\nrollback() {{\n  if [ -n \"$child\" ]; then kill \"$child\" 2>/dev/null || true; fi\n  rm -f \"$target\"\n  if [ -e \"$backup\" ]; then mv \"$backup\" \"$target\"; fi\n  printf 'rolled-back\\n' > \"$state\"\n  rm -f \"$lock\"\n  \"$target\" {arguments} >/dev/null 2>&1 &\n  rm -f \"$0\"\n  exit 1\n}}\nwhile kill -0 \"$parent\" 2>/dev/null; do sleep 1; done\nrm -f \"$health\" \"$backup\"\nprintf 'swapping\\n' > \"$state\"\nif [ -e \"$target\" ]; then mv \"$target\" \"$backup\"; fi\nif ! mv \"$candidate\" \"$target\"; then\n  if [ -e \"$backup\" ]; then mv \"$backup\" \"$target\"; fi\n  printf 'swap-failed\\n' > \"$state\"\n  rm -f \"$lock\"\n  exit 1\nfi\nchmod 755 \"$target\" || rollback\nMEDUSA_UPDATE_HEALTH_FILE=\"$health\" \"$target\" {arguments} &\nchild=$!\ni=0\nwhile [ \"$i\" -lt 100 ]; do\n  if [ -s \"$health\" ]; then\n    if [ -n \"$sequence_file\" ]; then\n      printf '%s\\n' \"$sequence_value\" > \"$sequence_file.tmp\" || rollback\n      mv \"$sequence_file.tmp\" \"$sequence_file\" || rollback\n    fi\n    printf 'healthy\\n' > \"$state\"\n    rm -f \"$backup\" \"$lock\" \"$0\"\n    exit 0\n  fi\n  if ! kill -0 \"$child\" 2>/dev/null; then break; fi\n  i=$((i + 1))\n  sleep 0.1\ndone\nrollback\n",
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
        .unwrap_or_else(|| "''".to_owned());
    let sequence_value = restart
        .rollout_sequence
        .map(|value| powershell_quote(&value.to_string()))
        .unwrap_or_else(|| "''".to_owned());
    format!(
        "$ErrorActionPreference = 'Stop'\r\n$parentPid = {parent_pid}\r\n$backup = {backup}\r\n$target = {target}\r\n$candidate = {candidate}\r\n$state = {state}\r\n$health = {health}\r\n$lock = {lock}\r\n$sequenceFile = {sequence_file}\r\n$sequenceValue = {sequence_value}\r\nfunction Restore-Previous([object]$Child) {{\r\n  if ($null -ne $Child) {{ Stop-Process -Id $Child.Id -Force -ErrorAction SilentlyContinue }}\r\n  Remove-Item $target -Force -ErrorAction SilentlyContinue\r\n  if (Test-Path -LiteralPath $backup) {{ Move-Item -LiteralPath $backup -Destination $target -Force }}\r\n  Set-Content -LiteralPath $state -Value 'rolled-back' -Encoding ascii\r\n  Remove-Item $lock -Force -ErrorAction SilentlyContinue\r\n  Start-Process -FilePath $target -ArgumentList @({arguments})\r\n  Remove-Item $PSCommandPath -Force -ErrorAction SilentlyContinue\r\n  exit 1\r\n}}\r\nwhile (Get-Process -Id $parentPid -ErrorAction SilentlyContinue) {{ Start-Sleep -Seconds 1 }}\r\nRemove-Item $health,$backup -Force -ErrorAction SilentlyContinue\r\nSet-Content -LiteralPath $state -Value 'swapping' -Encoding ascii\r\ntry {{\r\n  if (Test-Path -LiteralPath $target) {{ Move-Item -LiteralPath $target -Destination $backup -Force }}\r\n  Move-Item -LiteralPath $candidate -Destination $target -Force\r\n}} catch {{\r\n  if (Test-Path -LiteralPath $backup) {{ Move-Item -LiteralPath $backup -Destination $target -Force }}\r\n  Set-Content -LiteralPath $state -Value 'swap-failed' -Encoding ascii\r\n  Remove-Item $lock -Force -ErrorAction SilentlyContinue\r\n  exit 1\r\n}}\r\n$child = $null\r\ntry {{\r\n  $env:{health_env} = $health\r\n  $child = Start-Process -FilePath $target -ArgumentList @({arguments}) -PassThru\r\n  Remove-Item Env:{health_env} -ErrorAction SilentlyContinue\r\n  for ($i = 0; $i -lt 100; $i++) {{\r\n    if (Test-Path -LiteralPath $health) {{\r\n      if ($sequenceFile) {{\r\n        Set-Content -LiteralPath \"$sequenceFile.tmp\" -Value $sequenceValue -Encoding ascii\r\n        Move-Item -LiteralPath \"$sequenceFile.tmp\" -Destination $sequenceFile -Force\r\n      }}\r\n      Set-Content -LiteralPath $state -Value 'healthy' -Encoding ascii\r\n      Remove-Item $backup,$lock,$PSCommandPath -Force -ErrorAction SilentlyContinue\r\n      exit 0\r\n    }}\r\n    if ($child.HasExited) {{ break }}\r\n    Start-Sleep -Milliseconds 100\r\n    $child.Refresh()\r\n  }}\r\n}} catch {{\r\n  Remove-Item Env:{health_env} -ErrorAction SilentlyContinue\r\n  Restore-Previous $child\r\n}}\r\nRemove-Item Env:{health_env} -ErrorAction SilentlyContinue\r\nRestore-Previous $child\r\n",
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


def update_cli() -> None:
    path = Path("crates/medusa-cli/src/update_command.rs")
    text = path.read_text(encoding="utf-8")
    text = replace_once(
        text,
        '''    let updater = current.clone();
    let installed_sequence = read_installed_sequence(repo);''',
        '''    let updater = current.clone();
    let location = InstallLocation::current()?;
    let installed_sequence = read_installed_sequence(repo);''',
        "resolve installation before policy checks",
    )
    text = replace_once(
        text,
        '''    let location = InstallLocation::current()?;
    if let InstallKind::PackageManaged { manager, command } = location.kind {''',
        '''    if let InstallKind::PackageManaged { manager, command } = location.kind {''',
        "remove duplicate installation lookup",
    )
    text = replace_once(
        text,
        '''    let restart = Restart {
        arguments: vec![
            "--repo".to_owned(),
            repo.to_string_lossy().into_owned(),
            "--continue".to_owned(),
        ],
    };
    super::request_daemon_shutdown(repo);
    let scheduled = installer.schedule_replace(&candidate, &restart, std::process::id())?;
    staging_timer.finish("atomic-handoff-staged", Some(artifact.bytes), None)?;''',
        '''    let restart = Restart {
        arguments: vec![
            "--repo".to_owned(),
            repo.to_string_lossy().into_owned(),
            "--continue".to_owned(),
        ],
        sequence_file: Some(repo.join(".medusa/update-sequence")),
        rollout_sequence: Some(release.rollout_sequence),
    };
    let scheduled = installer.schedule_replace(&candidate, &restart, std::process::id())?;
    staging_timer.finish("atomic-handoff-staged", Some(artifact.bytes), None)?;
    super::request_daemon_shutdown(repo);''',
        "stage before daemon shutdown and defer sequence",
    )
    text = replace_once(
        text,
        '''    persist_sequence(repo, release.rollout_sequence)?;
    println!(''',
        '''    println!(''',
        "remove premature sequence persistence",
    )
    start = text.index("\nfn persist_sequence(")
    end = text.index("\nfn invalid(", start)
    text = text[:start] + "\n" + text[end:]
    text = replace_once(
        text,
        '''
    #[test]
    fn sequence_state_round_trips() {
        let directory = tempfile::tempdir().expect("tempdir");
        persist_sequence(directory.path(), 42).expect("sequence");
        assert_eq!(read_installed_sequence(directory.path()), Some(42));
    }
''',
        "\n",
        "premature sequence unit test",
    )
    path.write_text(text, encoding="utf-8")


if __name__ == "__main__":
    update_install()
    update_cli()
