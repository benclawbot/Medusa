#!/usr/bin/env python3
"""Preserve the updater public API while moving its trust model to v2."""

from __future__ import annotations

from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise RuntimeError(f"{label}: expected one match, found {count}")
    return text.replace(old, new, 1)


def update_manifest() -> None:
    path = Path("crates/medusa-update/src/manifest.rs")
    text = path.read_text(encoding="utf-8")
    text = replace_once(
        text,
        '''impl OperatingSystem {
    pub fn current() -> Result<Self, ManifestError> {''',
        '''impl From<&str> for OperatingSystem {
    fn from(value: &str) -> Self {
        match value {
            "linux" => Self::Linux,
            "macos" => Self::Macos,
            "windows" => Self::Windows,
            other => panic!("unsupported operating system literal {other}"),
        }
    }
}

impl OperatingSystem {
    pub fn current() -> Result<Self, ManifestError> {''',
        "operating system compatibility conversion",
    )
    text = replace_once(
        text,
        '''impl Architecture {
    pub fn current() -> Result<Self, ManifestError> {''',
        '''impl From<&str> for Architecture {
    fn from(value: &str) -> Self {
        match value {
            "x86_64" => Self::X86_64,
            "aarch64" => Self::Aarch64,
            other => panic!("unsupported architecture literal {other}"),
        }
    }
}

impl Architecture {
    pub fn current() -> Result<Self, ManifestError> {''',
        "architecture compatibility conversion",
    )
    path.write_text(text, encoding="utf-8")


def update_github() -> None:
    path = Path("crates/medusa-update/src/github.rs")
    text = path.read_text(encoding="utf-8")
    text = replace_once(
        text,
        '''    pub fn public() -> MedusaResult<Self> {
        Self::new("benclawbot/Medusa", GITHUB_API, TrustStore::production())
    }

    pub fn new(
        repository: impl Into<String>,
        api_base: impl Into<String>,
        trust_store: TrustStore,
    ) -> MedusaResult<Self> {''',
        '''    pub fn public() -> MedusaResult<Self> {
        Self::new("benclawbot/Medusa", GITHUB_API)
    }

    pub fn new(
        repository: impl Into<String>,
        api_base: impl Into<String>,
    ) -> MedusaResult<Self> {
        Self::with_trust_store(repository, api_base, TrustStore::production())
    }

    pub fn with_trust_store(
        repository: impl Into<String>,
        api_base: impl Into<String>,
        trust_store: TrustStore,
    ) -> MedusaResult<Self> {''',
        "release client constructors",
    )
    text = replace_once(
        text,
        '''        let client = GithubReleaseClient::new(
            "octo/medusa",
            "https://github.example/api/v3",
            TrustStore::production(),
        )''',
        '''        let client = GithubReleaseClient::with_trust_store(
            "octo/medusa",
            "https://github.example/api/v3",
            TrustStore::production(),
        )''',
        "custom trust constructor fixture",
    )
    path.write_text(text, encoding="utf-8")


def update_model() -> None:
    path = Path("crates/medusa-update/src/model.rs")
    text = path.read_text(encoding="utf-8")
    text = replace_once(
        text,
        "use std::{fs::File, io::Read, path::Path, time::Duration};",
        "use std::{fs, fs::File, io::{Read, Write}, path::Path, time::Duration};",
        "model IO imports",
    )
    text = replace_once(
        text,
        '''    pub fn artifact_for(&self, platform: Platform) -> MedusaResult<&Artifact> {
        let matches = self
            .artifacts
            .iter()
            .filter(|artifact| {
                artifact.kind == ArtifactKind::CliArchive && artifact.platform == platform
            })''',
        '''    pub fn artifact_for(&self, platform: &Platform) -> MedusaResult<&Artifact> {
        let matches = self
            .artifacts
            .iter()
            .filter(|artifact| {
                artifact.kind == ArtifactKind::CliArchive && artifact.platform == *platform
            })''',
        "artifact selection reference API",
    )
    text = replace_once(
        text,
        '''/// Validates byte count and SHA-256 without reading the whole artifact into memory.
pub fn verify_artifact''',
        '''/// Streams a reader to a durable file while reporting bounded progress.
pub fn copy_with_progress(
    reader: &mut impl Read,
    destination: &Path,
    expected_bytes: Option<u64>,
    mut progress: impl FnMut(u64, Option<u64>),
) -> MedusaResult<u64> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut output = File::create(destination)?;
    let mut buffer = [0_u8; 64 * 1024];
    let mut written = 0_u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        written = written
            .checked_add(read as u64)
            .ok_or_else(|| invalid("copy byte count overflow"))?;
        if expected_bytes.is_some_and(|expected| written > expected) {
            return Err(invalid("copy exceeded expected byte count"));
        }
        output.write_all(&buffer[..read])?;
        progress(written, expected_bytes);
    }
    output.sync_all()?;
    if expected_bytes.is_some_and(|expected| written != expected) {
        return Err(invalid(format!(
            "copy byte count mismatch: expected {}, got {written}",
            expected_bytes.unwrap_or_default()
        )));
    }
    Ok(written)
}

/// Validates byte count and SHA-256 without reading the whole artifact into memory.
pub fn verify_artifact''',
        "stream copy compatibility helper",
    )
    path.write_text(text, encoding="utf-8")


def update_exports() -> None:
    path = Path("crates/medusa-update/src/lib.rs")
    text = path.read_text(encoding="utf-8")
    text = replace_once(
        text,
        '''pub use model::{
    verify_artifact, verify_sha256, Artifact, DownloadReport, Release, UpdateCheck, UpdatePolicy,
};''',
        '''pub use model::{
    copy_with_progress, verify_artifact, verify_sha256, Artifact, DownloadReport, Release,
    UpdateCheck, UpdatePolicy,
};''',
        "copy helper export",
    )
    path.write_text(text, encoding="utf-8")


def update_cli() -> None:
    path = Path("crates/medusa-cli/src/update_command.rs")
    text = path.read_text(encoding="utf-8")
    text = replace_once(
        text,
        "    let artifact = release.artifact_for(platform)?;",
        "    let artifact = release.artifact_for(&platform)?;",
        "CLI artifact selection reference",
    )
    path.write_text(text, encoding="utf-8")


if __name__ == "__main__":
    update_manifest()
    update_github()
    update_model()
    update_exports()
    update_cli()
