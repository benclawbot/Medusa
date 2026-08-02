#!/usr/bin/env python3
"""Apply the health-atomic lifecycle correction with unique CLI anchors."""

from __future__ import annotations

import importlib.util
from pathlib import Path

SCRIPT = Path(__file__).with_name("apply-655-lifecycle.py")
SPEC = importlib.util.spec_from_file_location("apply_655_lifecycle", SCRIPT)
assert SPEC and SPEC.loader
LEGACY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LEGACY)


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise RuntimeError(f"{label}: expected one match, found {count}")
    return text.replace(old, new, 1)


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
        '''    require_automatic_for_unattended(automatic)?;

    let location = InstallLocation::current()?;
    if let InstallKind::PackageManaged { manager, command } = location.kind {''',
        '''    require_automatic_for_unattended(automatic)?;

    if let InstallKind::PackageManaged { manager, command } = location.kind {''',
        "remove later installation lookup",
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


def update_main_health_acknowledgement() -> None:
    path = Path("crates/medusa-cli/src/main.rs")
    text = path.read_text(encoding="utf-8")
    text = replace_once(
        text,
        '''fn run() -> MedusaResult<()> {
    medusa_update::acknowledge_update_health()?;
    let cli = Cli::parse();''',
        '''fn run() -> MedusaResult<()> {
    let cli = Cli::parse();''',
        "remove premature health acknowledgement",
    )
    text = replace_once(
        text,
        '''        oauth_preflight::run_if_needed(&config)?;
        let mut options = TuiOptions::for_repo(repo);''',
        '''        oauth_preflight::run_if_needed(&config)?;
        medusa_update::acknowledge_update_health()?;
        let mut options = TuiOptions::for_repo(repo);''',
        "acknowledge only after interactive startup prerequisites",
    )
    path.write_text(text, encoding="utf-8")


if __name__ == "__main__":
    LEGACY.update_install()
    update_cli()
    update_main_health_acknowledgement()
