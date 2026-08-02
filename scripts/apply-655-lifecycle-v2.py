#!/usr/bin/env python3
"""Finalize the health-atomic updater lifecycle and its validation fixes."""

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


def update_install_lints() -> None:
    path = Path("crates/medusa-update/src/install.rs")
    text = path.read_text(encoding="utf-8")
    text = replace_once(
        text,
        "fn unix_replace_script(\n",
        "// Atomic replacement scripts intentionally receive every persisted path explicitly.\n"
        "#[allow(clippy::too_many_arguments)]\n"
        "fn unix_replace_script(\n",
        "Unix replacement-script lint scope",
    )
    text = replace_once(
        text,
        "fn windows_replace_script(\n",
        "// Atomic replacement scripts intentionally receive every persisted path explicitly.\n"
        "#[allow(clippy::too_many_arguments)]\n"
        "fn windows_replace_script(\n",
        "Windows replacement-script lint scope",
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


def update_github_module() -> None:
    path = Path("crates/medusa-update/src/github.rs")
    text = path.read_text(encoding="utf-8")
    text = replace_once(
        text,
        "    fs::{self, File, OpenOptions},",
        "    fs::{self, OpenOptions},",
        "move GitHub file helpers",
    )
    text = replace_once(
        text,
        "use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};",
        "use medusa_core::MedusaResult;",
        "narrow GitHub error imports",
    )
    text = replace_once(
        text,
        "use serde::Deserialize;\n",
        "",
        "move GitHub response derives",
    )
    text = replace_once(
        text,
        '''    model::{invalid, verify_artifact},
};

const GITHUB_API''',
        '''    model::{invalid, verify_artifact},
};

mod support;

use support::{
    GithubAsset, GithubRelease, atomic_write, http_error, read_bounded, sync_parent,
};

const GITHUB_API''',
        "wire GitHub support module",
    )
    text = replace_once(
        text,
        "const DOWNLOAD_ATTEMPTS: u32 = 3;\n",
        "const DOWNLOAD_ATTEMPTS: u32 = 3;\nconst _: () = assert!(MAX_SIGNATURE < MAX_MANIFEST);\n",
        "compile-time release size invariant",
    )

    support_start = text.index("#[derive(Deserialize)]\nstruct GithubRelease")
    tests_start = text.index("\n#[cfg(test)]\nmod tests", support_start)
    text = text[:support_start] + text[tests_start + 1 :]
    text = replace_once(
        text,
        '''
    #[test]
    fn bounded_reader_rejects_truncated_limit_overrun() {
        let _ = MAX_MANIFEST;
        assert!(MAX_SIGNATURE < MAX_MANIFEST);
    }
''',
        "\n",
        "replace constant-only GitHub test",
    )
    if len(text.splitlines()) > 400:
        raise RuntimeError("github.rs remains above the 400-line module limit")
    path.write_text(text, encoding="utf-8")

    support_path = Path("crates/medusa-update/src/github/support.rs")
    support_path.parent.mkdir(parents=True, exist_ok=True)
    support_path.write_text(
        '''use std::{
    fs::{self, File},
    io::{Read, Write},
    path::Path,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use reqwest::blocking::Response;
use serde::Deserialize;

use crate::model::invalid;

#[derive(Deserialize)]
pub(super) struct GithubRelease {
    pub(super) tag_name: String,
    pub(super) draft: bool,
    pub(super) prerelease: bool,
    pub(super) assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
pub(super) struct GithubAsset {
    pub(super) name: String,
    pub(super) browser_download_url: String,
    pub(super) size: u64,
}

pub(super) fn read_bounded(
    mut response: Response,
    maximum: usize,
    label: &str,
) -> MedusaResult<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(invalid(format!("{label} exceeds {maximum} bytes")));
    }
    let mut body = Vec::new();
    response
        .by_ref()
        .take(maximum as u64 + 1)
        .read_to_end(&mut body)
        .map_err(http_error)?;
    if body.len() > maximum {
        return Err(invalid(format!("{label} exceeds {maximum} bytes")));
    }
    Ok(body)
}

pub(super) fn atomic_write(path: &Path, bytes: &[u8]) -> MedusaResult<()> {
    let temporary = path.with_extension("tmp");
    {
        let mut file = File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&temporary, path)?;
    sync_parent(path)
}

pub(super) fn sync_parent(path: &Path) -> MedusaResult<()> {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub(super) fn http_error(error: impl std::fmt::Display) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Transient,
        format!("GitHub release request failed: {error}"),
    )
}
''',
        encoding="utf-8",
    )


if __name__ == "__main__":
    LEGACY.update_install()
    update_install_lints()
    update_cli()
    update_main_health_acknowledgement()
    update_github_module()
