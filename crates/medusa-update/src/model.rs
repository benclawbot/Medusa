use std::{
    fs,
    fs::File,
    io::{Read, Write},
    path::Path,
    time::Duration,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::manifest::{
    ArtifactKind, BuildSource, ManifestArtifact, Platform, ReleaseManifest, RolloutPolicy,
    VerifiedManifest,
};
use crate::release_id::ReleaseId;

/// A release asset whose URL and integrity metadata came from a verified manifest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Artifact {
    pub name: String,
    pub browser_download_url: String,
    pub bytes: u64,
    pub sha256: String,
    pub kind: ArtifactKind,
    pub platform: Platform,
    pub target: String,
}

impl Artifact {
    pub(crate) fn from_manifest(entry: &ManifestArtifact, browser_download_url: String) -> Self {
        Self {
            name: entry.name.clone(),
            browser_download_url,
            bytes: entry.bytes,
            sha256: entry.sha256.clone(),
            kind: entry.kind,
            platform: entry.platform,
            target: entry.target.clone(),
        }
    }
}

/// A stable GitHub release authorized by an embedded Medusa Ed25519 key.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Release {
    pub version: Version,
    pub release_id: ReleaseId,
    pub repository: String,
    pub source: BuildSource,
    pub minimum_updater_version: Version,
    pub rollout_sequence: u64,
    pub rollout_percentage: u8,
    pub signing_key_id: String,
    pub manifest_sha256: String,
    pub artifacts: Vec<Artifact>,
}

impl Release {
    pub(crate) fn from_verified(
        repository: String,
        verified: VerifiedManifest,
        artifacts: Vec<Artifact>,
    ) -> Self {
        let VerifiedManifest {
            manifest,
            key_id,
            manifest_sha256,
        } = verified;
        let ReleaseManifest {
            version,
            release_id,
            minimum_updater_version,
            source,
            rollout:
                RolloutPolicy {
                    sequence,
                    percentage,
                    ..
                },
            ..
        } = manifest;
        let release_id = release_id.unwrap_or_else(|| ReleaseId::from_version(&version));
        Self {
            version,
            release_id,
            repository,
            source,
            minimum_updater_version,
            rollout_sequence: sequence,
            rollout_percentage: percentage,
            signing_key_id: key_id,
            manifest_sha256,
            artifacts,
        }
    }

    pub fn artifact_for(&self, platform: &Platform) -> MedusaResult<&Artifact> {
        let matches = self
            .artifacts
            .iter()
            .filter(|artifact| {
                artifact.kind == ArtifactKind::CliArchive && artifact.platform == *platform
            })
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [artifact] => Ok(*artifact),
            [] => Err(invalid(format!(
                "release {} has no CLI artifact for {:?}/{:?}",
                self.version, platform.os, platform.architecture
            ))),
            _ => Err(invalid(format!(
                "release {} has multiple CLI artifacts for {:?}/{:?}",
                self.version, platform.os, platform.architecture
            ))),
        }
    }
}

/// The updater's explicit policy. Automatic updates still verify every byte.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UpdatePolicy {
    #[default]
    Manual,
    Check,
    Automatic,
}

impl UpdatePolicy {
    #[must_use]
    pub fn from_environment() -> Self {
        match std::env::var("MEDUSA_UPDATE_POLICY").ok().as_deref() {
            Some("automatic") => Self::Automatic,
            Some("check") => Self::Check,
            _ => Self::Manual,
        }
    }
}

/// Result of comparing the running binary version with a release version.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum UpdateCheck {
    UpToDate { current: Version },
    Available { current: Version, latest: Version },
    CurrentBuildUnparseable { current: String, latest: Version },
}

impl UpdateCheck {
    #[must_use]
    pub fn compare(current: &str, latest: Version) -> Self {
        match Version::parse(current.trim_start_matches('v')) {
            Ok(current) if current >= latest => Self::UpToDate { current },
            Ok(current) => Self::Available { current, latest },
            Err(_) => Self::CurrentBuildUnparseable {
                current: current.to_owned(),
                latest,
            },
        }
    }

    #[must_use]
    pub fn update_available(&self) -> bool {
        matches!(
            self,
            Self::Available { .. } | Self::CurrentBuildUnparseable { .. }
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DownloadReport {
    pub bytes: u64,
    pub retries: u32,
    pub elapsed_ms: u64,
}

impl DownloadReport {
    #[must_use]
    pub fn new(bytes: u64, retries: u32, elapsed: Duration) -> Self {
        Self {
            bytes,
            retries,
            elapsed_ms: elapsed.as_millis().try_into().unwrap_or(u64::MAX),
        }
    }
}

/// Streams a reader to a durable file while reporting bounded progress.
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
pub fn verify_artifact(
    path: &Path,
    expected_bytes: u64,
    expected_sha256: &str,
) -> MedusaResult<()> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut bytes = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| invalid("artifact byte count overflow"))?;
        if bytes > expected_bytes {
            return Err(invalid(format!(
                "artifact is larger than the signed byte count {expected_bytes}"
            )));
        }
        digest.update(&buffer[..read]);
    }
    if bytes != expected_bytes {
        return Err(invalid(format!(
            "artifact byte count mismatch: expected {expected_bytes}, got {bytes}"
        )));
    }
    let actual = hex::encode(digest.finalize());
    if !actual.eq_ignore_ascii_case(expected_sha256.trim()) {
        return Err(invalid(format!(
            "artifact SHA-256 mismatch: expected {expected_sha256}, got {actual}"
        )));
    }
    Ok(())
}

/// Compatibility wrapper retained for callers that only have a digest.
pub fn verify_sha256(path: &Path, expected: &str) -> MedusaResult<()> {
    let bytes = std::fs::metadata(path)?.len();
    verify_artifact(path, bytes, expected)
}

pub(crate) fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn semantic_versions_do_not_use_lexicographic_ordering() {
        assert!(matches!(
            UpdateCheck::compare("1.9.0", Version::parse("1.10.0").expect("version")),
            UpdateCheck::Available { .. }
        ));
        assert!(matches!(
            UpdateCheck::compare("v2.0.0", Version::parse("1.99.0").expect("version")),
            UpdateCheck::UpToDate { .. }
        ));
    }

    #[test]
    fn truncated_or_tampered_artifact_fails() {
        let directory = tempfile::tempdir().expect("tempdir");
        let artifact = directory.path().join("artifact");
        fs::write(&artifact, b"safe release").expect("artifact");
        assert!(verify_artifact(&artifact, 100, "00").is_err());
        assert!(verify_artifact(&artifact, 12, &"00".repeat(32)).is_err());
    }
}
