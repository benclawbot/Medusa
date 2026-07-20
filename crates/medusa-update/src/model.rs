use std::{fs, path::Path, process::Command};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Installation target selected from the host OS and CPU architecture.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Platform {
    pub os: String,
    pub architecture: String,
}

impl Platform {
    #[must_use]
    pub fn current() -> Self {
        Self {
            os: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
        }
    }

    #[must_use]
    pub fn cli_asset_name(&self) -> &'static str {
        match self.os.as_str() {
            "windows" => "medusa-cli-windows.zip",
            "macos" => "medusa-cli-macos.tar.gz",
            _ => "medusa-cli-linux.tar.gz",
        }
    }
}

/// A file published in a GitHub release and recorded in its integrity manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Artifact {
    pub name: String,
    pub browser_download_url: String,
    pub bytes: u64,
    pub sha256: String,
}

/// A verified, non-draft GitHub release.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Release {
    pub version: Version,
    pub repository: String,
    pub manifest: Artifact,
    pub artifacts: Vec<Artifact>,
}

impl Release {
    pub fn artifact_for(&self, platform: &Platform) -> MedusaResult<&Artifact> {
        let expected = platform.cli_asset_name();
        self.artifacts
            .iter()
            .find(|artifact| artifact.name == expected)
            .ok_or_else(|| {
                invalid(format!(
                    "release {} does not include {expected}",
                    self.version
                ))
            })
    }
}

/// The updater's explicit policy. Automatic updates still verify every release.
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

/// Verifies the signed release manifest before a release asset is trusted.
pub trait AttestationVerifier {
    fn verify_manifest(&self, manifest: &Path, repository: &str) -> MedusaResult<()>;
}

/// Uses GitHub's Sigstore-backed artifact attestation verifier when available.
pub struct GithubAttestationVerifier;

impl AttestationVerifier for GithubAttestationVerifier {
    fn verify_manifest(&self, manifest: &Path, repository: &str) -> MedusaResult<()> {
        let status = Command::new("gh")
            .args(["attestation", "verify", "--repo", repository])
            .arg(manifest)
            .status()
            .map_err(|error| {
                MedusaError::new(
                    ErrorCode::DependencyUnavailable,
                    ErrorCategory::Environment,
                    format!("GitHub CLI attestation verifier is unavailable: {error}"),
                )
            })?;
        if status.success() {
            Ok(())
        } else {
            Err(invalid("GitHub artifact attestation verification failed"))
        }
    }
}

/// Computes and validates an artifact digest before it is extracted or installed.
pub fn verify_sha256(path: &Path, expected: &str) -> MedusaResult<()> {
    let bytes = fs::read(path)?;
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual.eq_ignore_ascii_case(expected.trim()) {
        Ok(())
    } else {
        Err(invalid(format!(
            "SHA-256 mismatch for {}: expected {expected}, got {actual}",
            path.display()
        )))
    }
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
    fn digest_mismatch_fails_before_installation() {
        let directory = tempfile::tempdir().expect("tempdir");
        let artifact = directory.path().join("artifact");
        fs::write(&artifact, b"safe release").expect("artifact");
        assert!(verify_sha256(&artifact, "00").is_err());
    }
}
