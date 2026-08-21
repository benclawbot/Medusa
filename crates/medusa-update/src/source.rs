use std::{path::Path, sync::Mutex};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use reqwest::{blocking::Client, StatusCode};
use serde::Deserialize;

use crate::{
    copy_with_progress, verify_artifact, Architecture, AtomicInstaller, OperatingSystem, Platform,
    Restart, ScheduledUpdate,
};

const GITHUB_API: &str = "https://api.github.com";
const REPOSITORY: &str = "benclawbot/Medusa";
const BRANCH: &str = "main";
const ROLLING_ASSET_BASE: &str =
    "https://github.com/benclawbot/Medusa/releases/download/main-latest";
const ROLLING_MANIFEST_SCHEMA: &str = "medusa-main-artifact-v1";
const SHA1_HEX_LENGTH: usize = 40;
const SHA256_HEX_LENGTH: usize = 64;

/// The immutable revision currently at the head of Medusa's main branch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MainBranchRevision {
    pub sha: String,
}

/// Discovers main-branch revisions and stages exact-revision rolling prebuilt binaries.
pub struct MainBranchUpdater {
    client: Client,
    api_base: String,
    asset_base: String,
    verified_revision: Mutex<Option<String>>,
}

impl MainBranchUpdater {
    pub fn public() -> MedusaResult<Self> {
        Self::new(GITHUB_API)
    }

    pub fn new(api_base: impl Into<String>) -> MedusaResult<Self> {
        Self::with_asset_base(api_base, ROLLING_ASSET_BASE)
    }

    fn with_asset_base(
        api_base: impl Into<String>,
        asset_base: impl Into<String>,
    ) -> MedusaResult<Self> {
        Ok(Self {
            client: Client::builder()
                .user_agent("medusa-updater")
                .build()
                .map_err(http_error)?,
            api_base: api_base.into().trim_end_matches('/').to_owned(),
            asset_base: asset_base.into().trim_end_matches('/').to_owned(),
            verified_revision: Mutex::new(None),
        })
    }

    pub fn latest_main(&self) -> MedusaResult<MainBranchRevision> {
        let url = format!("{}/repos/{REPOSITORY}/commits/{BRANCH}", self.api_base);
        let revision: GithubCommit = self
            .client
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .map_err(http_error)?
            .error_for_status()
            .map_err(http_error)?
            .json()
            .map_err(http_error)?;
        validate_revision(&revision.sha)?;
        *self
            .verified_revision
            .lock()
            .map_err(|_| internal_error("verified update revision lock is poisoned"))? =
            Some(revision.sha.clone());
        Ok(MainBranchRevision { sha: revision.sha })
    }

    /// Downloads the rolling prebuilt binary for the exact verified `main` revision,
    /// verifies its byte count and SHA-256, and stages the existing atomic handoff.
    pub fn schedule_main_install(
        &self,
        executable: &Path,
        repo: &Path,
        parent_pid: u32,
        progress: impl FnMut(u64, Option<u64>),
    ) -> MedusaResult<ScheduledUpdate> {
        let revision = self.verified_revision()?;
        let platform = Platform::current().map_err(|error| {
            invalid(format!(
                "cannot select rolling main artifact for this platform: {error}"
            ))
        })?;
        let asset_name = rolling_asset_name(platform, &revision)?;
        let manifest = self.fetch_artifact_manifest(&asset_name, &revision)?;

        let workspace = tempfile::Builder::new()
            .prefix("medusa-main-update-")
            .tempdir()?;
        let archive = workspace.path().join(&asset_name);
        let mut response = self.asset_response(&asset_name, &revision)?;
        copy_with_progress(&mut response, &archive, Some(manifest.bytes), progress)?;
        verify_artifact(&archive, manifest.bytes, &manifest.sha256)?;

        let installer = AtomicInstaller::new(executable.to_path_buf());
        let candidate = installer.extract_archive(&archive, &workspace.path().join("extract"))?;
        let restart = Restart {
            arguments: vec![
                "--repo".to_owned(),
                repo.to_string_lossy().into_owned(),
                "--fresh".to_owned(),
            ],
            sequence_file: None,
            rollout_sequence: None,
        };
        installer.schedule_replace(&candidate, &restart, parent_pid)
    }

    fn verified_revision(&self) -> MedusaResult<String> {
        self.verified_revision
            .lock()
            .map_err(|_| internal_error("verified update revision lock is poisoned"))?
            .clone()
            .ok_or_else(|| {
                MedusaError::new(
                    ErrorCode::InternalInvariant,
                    ErrorCategory::Internal,
                    "cannot stage a main update before verifying a revision",
                )
            })
    }

    fn fetch_artifact_manifest(
        &self,
        asset_name: &str,
        revision: &str,
    ) -> MedusaResult<RollingMainArtifact> {
        let manifest_name = format!("{asset_name}.json");
        let manifest: RollingMainArtifact = self
            .asset_response(&manifest_name, revision)?
            .json()
            .map_err(asset_error)?;
        manifest.validate(revision, asset_name)?;
        Ok(manifest)
    }

    fn asset_response(
        &self,
        asset_name: &str,
        revision: &str,
    ) -> MedusaResult<reqwest::blocking::Response> {
        let url = format!("{}/{}", self.asset_base, asset_name);
        let response = self.client.get(url).send().map_err(asset_error)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Transient,
                format!(
                    "prebuilt main artifact for revision {revision} is still publishing; retry shortly"
                ),
            ));
        }
        response.error_for_status().map_err(asset_error)
    }
}

#[derive(Deserialize)]
struct GithubCommit {
    sha: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct RollingMainArtifact {
    schema: String,
    revision: String,
    name: String,
    bytes: u64,
    sha256: String,
}

impl RollingMainArtifact {
    fn validate(&self, expected_revision: &str, expected_name: &str) -> MedusaResult<()> {
        if self.schema != ROLLING_MANIFEST_SCHEMA {
            return Err(invalid(format!(
                "unsupported rolling main artifact schema {}",
                self.schema
            )));
        }
        validate_revision(&self.revision)?;
        if self.revision != expected_revision {
            return Err(MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Transient,
                format!(
                    "prebuilt main artifact is for revision {}, but main is {}; retry shortly",
                    self.revision, expected_revision
                ),
            ));
        }
        if self.name != expected_name {
            return Err(invalid(
                "rolling main artifact manifest name does not match platform",
            ));
        }
        if self.bytes == 0 {
            return Err(invalid("rolling main artifact has an empty byte count"));
        }
        if self.sha256.len() != SHA256_HEX_LENGTH
            || !self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(invalid("rolling main artifact has an invalid SHA-256"));
        }
        Ok(())
    }
}

fn rolling_asset_name(platform: Platform, revision: &str) -> MedusaResult<String> {
    validate_revision(revision)?;
    let os = match platform.os {
        OperatingSystem::Linux => "linux",
        OperatingSystem::Macos => "macos",
        OperatingSystem::Windows => "windows",
    };
    let architecture = match platform.architecture {
        Architecture::X86_64 => "x86_64",
        Architecture::Aarch64 => "aarch64",
    };
    let extension = match platform.os {
        OperatingSystem::Windows => "zip",
        OperatingSystem::Linux | OperatingSystem::Macos => "tar.gz",
    };
    Ok(format!("medusa-main-{os}-{architecture}.{extension}"))
}

fn validate_revision(revision: &str) -> MedusaResult<()> {
    if !matches!(revision.len(), SHA1_HEX_LENGTH | SHA256_HEX_LENGTH)
        || !revision.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid("GitHub returned an invalid immutable revision"));
    }
    Ok(())
}

fn http_error(error: impl std::fmt::Display) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Transient,
        format!("GitHub main branch request failed: {error}"),
    )
}

fn asset_error(error: impl std::fmt::Display) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Transient,
        format!("GitHub rolling main artifact request failed: {error}"),
    )
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

fn internal_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        message,
    )
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use super::*;

    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn discovers_main_revision() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).expect("read request");
            let body = format!("{{\"sha\":\"{REVISION}\"}}");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body,
            )
            .expect("response");
        });
        assert_eq!(
            MainBranchUpdater::new(base)
                .expect("client")
                .latest_main()
                .expect("revision")
                .sha,
            REVISION
        );
        worker.join().expect("server");
    }

    #[test]
    fn rejects_invalid_main_revision() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).expect("read request");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 24\r\nConnection: close\r\n\r\n{\"sha\":\"main; echo pwn\"}")
                .expect("response");
        });
        assert!(
            MainBranchUpdater::new(base)
                .expect("client")
                .latest_main()
                .is_err()
        );
        worker.join().expect("server");
    }

    #[test]
    fn rolling_asset_selection_is_platform_specific() {
        let windows = Platform {
            os: OperatingSystem::Windows,
            architecture: Architecture::X86_64,
        };
        assert_eq!(
            rolling_asset_name(windows, REVISION).expect("asset"),
            "medusa-main-windows-x86_64.zip"
        );

        let macos_arm = Platform {
            os: OperatingSystem::Macos,
            architecture: Architecture::Aarch64,
        };
        assert_eq!(
            rolling_asset_name(macos_arm, REVISION).expect("asset"),
            "medusa-main-macos-aarch64.tar.gz"
        );
    }

    #[test]
    fn rolling_manifest_is_exact_revision_bound() {
        let manifest = RollingMainArtifact {
            schema: ROLLING_MANIFEST_SCHEMA.to_owned(),
            revision: REVISION.to_owned(),
            name: "medusa-main-windows-x86_64.zip".to_owned(),
            bytes: 42,
            sha256: "ab".repeat(32),
        };
        assert!(
            manifest
                .validate(REVISION, "medusa-main-windows-x86_64.zip")
                .is_ok()
        );
        assert!(
            manifest
                .validate(
                    "1123456789abcdef0123456789abcdef01234567",
                    "medusa-main-windows-x86_64.zip"
                )
                .is_err()
        );
    }

    #[test]
    fn normal_main_updater_contains_no_source_build_command() {
        let forbidden = ["cargo", "install"].join(" ");
        assert!(!include_str!("source.rs").contains(&forbidden));
    }
}
