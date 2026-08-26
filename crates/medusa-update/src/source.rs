use std::{
    collections::{HashSet, VecDeque},
    fs,
    io::{self, BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use reqwest::{
    StatusCode,
    blocking::{Client, Response},
};
use serde::Deserialize;

use crate::{
    Architecture, AtomicInstaller, OperatingSystem, Platform, Restart, ScheduledUpdate,
    copy_with_progress, verify_artifact,
};

const GITHUB_API: &str = "https://api.github.com";
const REPOSITORY: &str = "benclawbot/Medusa";
const BRANCH: &str = "main";
const REPOSITORY_URL: &str = "https://github.com/benclawbot/Medusa.git";
const ROLLING_ASSET_BASE: &str = "https://github.com/benclawbot/Medusa/releases/download";
const ROLLING_MANIFEST_SCHEMA: &str = "medusa-main-artifact-v1";
const ROLLING_PUBLISH_WAIT: Duration = Duration::from_secs(180);
const ROLLING_PUBLISH_POLL: Duration = Duration::from_secs(2);
// Keep individual requests bounded so a stalled socket cannot defeat the publication window.
const ROLLING_REQUEST_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const ROLLING_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const SHA1_HEX_LENGTH: usize = 40;
const SHA256_HEX_LENGTH: usize = 64;

/// The immutable revision currently at the head of Medusa's main branch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MainBranchRevision {
    pub sha: String,
}

/// Snapshot of the local Cargo build used when the rolling `main` artifact is unavailable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MainBuildProgress {
    pub compiled_packages: usize,
    pub current_package: Option<String>,
    pub elapsed: Duration,
}

/// Phase reported while a revision-scoped prebuilt main artifact is being staged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MainArtifactPhase {
    Waiting,
    Downloading,
    Verifying,
}

/// Snapshot of a revision-scoped prebuilt main artifact update.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MainArtifactProgress {
    pub phase: MainArtifactPhase,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub elapsed: Duration,
}

/// Discovers main-branch revisions, stages exact-revision rolling binaries, and can
/// compile locally when CI has not published the requested artifact yet.
pub struct MainBranchUpdater {
    client: Client,
    api_base: String,
    asset_base: String,
    request_timeout: Duration,
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
        Self::with_asset_base_and_timeouts(
            api_base,
            asset_base,
            ROLLING_REQUEST_CONNECT_TIMEOUT,
            ROLLING_REQUEST_TIMEOUT,
        )
    }

    fn with_asset_base_and_timeouts(
        api_base: impl Into<String>,
        asset_base: impl Into<String>,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> MedusaResult<Self> {
        Ok(Self {
            client: Client::builder()
                .user_agent("medusa-updater")
                .connect_timeout(connect_timeout)
                .timeout(request_timeout)
                .build()
                .map_err(http_error)?,
            api_base: api_base.into().trim_end_matches('/').to_owned(),
            asset_base: asset_base.into().trim_end_matches('/').to_owned(),
            request_timeout,
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
        mut progress: impl FnMut(u64, Option<u64>),
    ) -> MedusaResult<ScheduledUpdate> {
        self.schedule_main_install_with_progress(executable, repo, parent_pid, |snapshot| {
            if snapshot.phase == MainArtifactPhase::Downloading {
                progress(snapshot.downloaded, snapshot.total);
            }
        })
    }

    /// Downloads, verifies, and stages a prebuilt CLI for the exact verified `main` revision,
    /// reporting publication waits and download progress to the caller.
    pub fn schedule_main_install_with_progress(
        &self,
        executable: &Path,
        repo: &Path,
        parent_pid: u32,
        mut progress: impl FnMut(MainArtifactProgress),
    ) -> MedusaResult<ScheduledUpdate> {
        let revision = self.verified_revision()?;
        let platform = Platform::current().map_err(|error| {
            invalid(format!(
                "cannot select rolling main artifact for this platform: {error}"
            ))
        })?;
        let asset_name = rolling_asset_name(platform, &revision)?;
        let started = Instant::now();
        let manifest =
            self.fetch_artifact_manifest(&asset_name, &revision, started, &mut progress)?;

        let workspace = tempfile::Builder::new()
            .prefix("medusa-main-update-")
            .tempdir()?;
        let archive = workspace.path().join(&asset_name);
        let mut response =
            self.asset_response_with_progress(&asset_name, &revision, started, &mut progress)?;
        copy_with_progress(
            &mut response,
            &archive,
            Some(manifest.bytes),
            |downloaded, total| {
                progress(MainArtifactProgress {
                    phase: MainArtifactPhase::Downloading,
                    downloaded,
                    total,
                    elapsed: started.elapsed(),
                });
            },
        )?;
        progress(MainArtifactProgress {
            phase: MainArtifactPhase::Verifying,
            downloaded: manifest.bytes,
            total: Some(manifest.bytes),
            elapsed: started.elapsed(),
        });
        verify_artifact(&archive, manifest.bytes, &manifest.sha256)?;

        let installer = AtomicInstaller::new(executable.to_path_buf());
        let candidate = installer.extract_archive(&archive, &workspace.path().join("extract"))?;
        let restart = Restart {
            arguments: vec![
                "--repo".to_owned(),
                repo.to_string_lossy().into_owned(),
                "--fresh".to_owned(),
            ],
            detached: false,
            sequence_file: None,
            rollout_sequence: None,
        };
        installer.schedule_replace(&candidate, &restart, parent_pid)
    }

    /// Builds the exact verified `main` revision in an isolated Cargo root and stages the
    /// resulting executable for the existing rollback-aware atomic handoff.
    pub fn build_and_schedule_main_install(
        &self,
        executable: &Path,
        repo: &Path,
        parent_pid: u32,
        mut progress: impl FnMut(MainBuildProgress),
    ) -> MedusaResult<ScheduledUpdate> {
        let revision = self.verified_revision()?;
        validate_revision(&revision)?;
        ensure_cargo_available()?;

        let workspace = tempfile::Builder::new()
            .prefix("medusa-main-build-")
            .tempdir()?;
        let install_root = workspace.path().join("install");
        let target_dir = cargo_target_directory(repo);
        fs::create_dir_all(&target_dir)?;
        let started = Instant::now();
        let compiled_packages =
            run_cargo_install(&revision, &install_root, &target_dir, &mut progress)?;

        let candidate = install_root.join("bin").join(medusa_binary_name());
        let installer = AtomicInstaller::new(executable.to_path_buf());
        let restart = Restart {
            arguments: vec![
                "--repo".to_owned(),
                repo.to_string_lossy().into_owned(),
                "--fresh".to_owned(),
            ],
            detached: false,
            sequence_file: None,
            rollout_sequence: None,
        };
        let scheduled = installer.schedule_replace(&candidate, &restart, parent_pid)?;
        progress(MainBuildProgress {
            compiled_packages,
            current_package: None,
            elapsed: started.elapsed(),
        });
        Ok(scheduled)
    }

    /// Returns whether the exact desktop executable for a verified main revision has been
    /// published and has a valid revision-bound manifest. This is intentionally a single
    /// request: checking for an update must never wait for a CI publication race.
    pub fn main_desktop_artifact_available(&self, revision: &str) -> MedusaResult<bool> {
        validate_revision(revision)?;
        let platform = Platform::current().map_err(|error| {
            invalid(format!(
                "cannot select rolling desktop artifact for this platform: {error}"
            ))
        })?;
        let asset_name = rolling_desktop_asset_name(platform, revision)?;
        self.artifact_manifest_available(&asset_name, revision)
    }

    /// Returns whether the exact CLI executable for a verified main revision is published.
    pub fn main_cli_artifact_available(&self, revision: &str) -> MedusaResult<bool> {
        validate_revision(revision)?;
        let platform = Platform::current().map_err(|error| {
            invalid(format!(
                "cannot select rolling main artifact for this platform: {error}"
            ))
        })?;
        let asset_name = rolling_asset_name(platform, revision)?;
        self.artifact_manifest_available(&asset_name, revision)
    }

    /// Downloads, verifies, and stages the exact desktop executable for a verified main
    /// revision. The final replacement is delegated to the same rollback-aware atomic
    /// installer used by the CLI updater.
    pub fn schedule_main_desktop_install(
        &self,
        executable: &Path,
        restart: &Restart,
        parent_pid: u32,
        mut progress: impl FnMut(u64, Option<u64>),
    ) -> MedusaResult<ScheduledUpdate> {
        let revision = self.verified_revision()?;
        let platform = Platform::current().map_err(|error| {
            invalid(format!(
                "cannot select rolling desktop artifact for this platform: {error}"
            ))
        })?;
        let asset_name = rolling_desktop_asset_name(platform, &revision)?;
        let manifest = self.fetch_artifact_manifest_once(&asset_name, &revision)?;

        let workspace = tempfile::Builder::new()
            .prefix("medusa-desktop-main-update-")
            .tempdir()?;
        let artifact = workspace.path().join(&asset_name);
        let mut response = self.asset_response(&asset_name, &revision)?;
        copy_with_progress(
            &mut response,
            &artifact,
            Some(manifest.bytes),
            &mut progress,
        )?;
        verify_artifact(&artifact, manifest.bytes, &manifest.sha256)?;

        AtomicInstaller::new(executable.to_path_buf())
            .schedule_replace(&artifact, restart, parent_pid)
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
        started: Instant,
        progress: &mut impl FnMut(MainArtifactProgress),
    ) -> MedusaResult<RollingMainArtifact> {
        let manifest_name = format!("{asset_name}.json");
        let deadline = Instant::now() + ROLLING_PUBLISH_WAIT;
        loop {
            let mut waiting = || {
                progress(MainArtifactProgress {
                    phase: MainArtifactPhase::Waiting,
                    downloaded: 0,
                    total: None,
                    elapsed: started.elapsed(),
                });
            };
            let manifest: RollingMainArtifact = self
                .asset_response_until_with_progress(
                    &manifest_name,
                    revision,
                    deadline,
                    &mut waiting,
                )?
                .json()
                .map_err(asset_error)?;
            validate_revision(&manifest.revision)?;
            if manifest.revision == revision {
                manifest.validate(revision, asset_name)?;
                return Ok(manifest);
            }
            if Instant::now() >= deadline {
                return Err(publish_timeout(revision));
            }
            thread::sleep(ROLLING_PUBLISH_POLL);
        }
    }

    fn artifact_manifest_available(&self, asset_name: &str, revision: &str) -> MedusaResult<bool> {
        let manifest_name = format!("{asset_name}.json");
        let Some(response) = self.asset_response_once(&manifest_name, revision)? else {
            return Ok(false);
        };
        let manifest: RollingMainArtifact = response.json().map_err(asset_error)?;
        validate_revision(&manifest.revision)?;
        match manifest.validate(revision, asset_name) {
            Ok(()) => Ok(true),
            Err(error) if error.code == ErrorCode::DependencyUnavailable => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn fetch_artifact_manifest_once(
        &self,
        asset_name: &str,
        revision: &str,
    ) -> MedusaResult<RollingMainArtifact> {
        let manifest_name = format!("{asset_name}.json");
        let Some(response) = self.asset_response_once(&manifest_name, revision)? else {
            return Err(not_published(revision));
        };
        let manifest: RollingMainArtifact = response.json().map_err(asset_error)?;
        manifest.validate(revision, asset_name)?;
        Ok(manifest)
    }

    fn asset_response(
        &self,
        asset_name: &str,
        revision: &str,
    ) -> MedusaResult<reqwest::blocking::Response> {
        self.asset_response_until(asset_name, revision, Instant::now() + ROLLING_PUBLISH_WAIT)
    }

    fn asset_response_with_progress(
        &self,
        asset_name: &str,
        revision: &str,
        started: Instant,
        progress: &mut impl FnMut(MainArtifactProgress),
    ) -> MedusaResult<reqwest::blocking::Response> {
        let deadline = Instant::now() + ROLLING_PUBLISH_WAIT;
        let mut waiting = || {
            progress(MainArtifactProgress {
                phase: MainArtifactPhase::Waiting,
                downloaded: 0,
                total: None,
                elapsed: started.elapsed(),
            });
        };
        self.asset_response_until_with_progress(asset_name, revision, deadline, &mut waiting)
    }

    fn asset_response_until(
        &self,
        asset_name: &str,
        revision: &str,
        deadline: Instant,
    ) -> MedusaResult<reqwest::blocking::Response> {
        self.asset_response_until_with_progress(asset_name, revision, deadline, || {})
    }

    fn asset_response_until_with_progress(
        &self,
        asset_name: &str,
        revision: &str,
        deadline: Instant,
        mut waiting: impl FnMut(),
    ) -> MedusaResult<reqwest::blocking::Response> {
        loop {
            waiting();
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(publish_timeout(revision));
            }
            if let Some(response) = self.asset_response_once_with_timeout(
                asset_name,
                revision,
                remaining.min(self.request_timeout),
            )? {
                return Ok(response);
            }
            thread::sleep(ROLLING_PUBLISH_POLL);
        }
    }

    fn asset_response_once(
        &self,
        asset_name: &str,
        revision: &str,
    ) -> MedusaResult<Option<Response>> {
        self.asset_response_once_with_timeout(asset_name, revision, self.request_timeout)
    }

    fn asset_response_once_with_timeout(
        &self,
        asset_name: &str,
        revision: &str,
        timeout: Duration,
    ) -> MedusaResult<Option<Response>> {
        let release_tag = rolling_release_tag(revision)?;
        let url = format!("{}/{}/{}", self.asset_base, release_tag, asset_name);
        let response = self
            .client
            .get(&url)
            .timeout(timeout)
            .send()
            .map_err(asset_error)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        response.error_for_status().map(Some).map_err(asset_error)
    }
}

fn rolling_release_tag(revision: &str) -> MedusaResult<String> {
    validate_revision(revision)?;
    Ok(format!("main-{revision}"))
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
                    "prebuilt main artifact is for revision {}, but main is {}",
                    self.revision, expected_revision
                ),
            )
            .with_retryable(true));
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
    let (os, architecture, extension) = match (platform.os, platform.architecture) {
        (OperatingSystem::Linux, Architecture::X86_64) => ("linux", "x86_64", "tar.gz"),
        (OperatingSystem::Macos, Architecture::Aarch64) => ("macos", "aarch64", "tar.gz"),
        (OperatingSystem::Windows, Architecture::X86_64) => ("windows", "x86_64", "zip"),
        _ => {
            return Err(invalid(
                "rolling main prebuilt updates are not published for this OS/architecture",
            ));
        }
    };
    Ok(format!("medusa-main-{os}-{architecture}.{extension}"))
}

pub fn rolling_desktop_asset_name(platform: Platform, revision: &str) -> MedusaResult<String> {
    validate_revision(revision)?;
    let (os, architecture, extension) = match (platform.os, platform.architecture) {
        (OperatingSystem::Linux, Architecture::X86_64) => ("linux", "x86_64", ""),
        (OperatingSystem::Macos, Architecture::Aarch64) => ("macos", "aarch64", ""),
        (OperatingSystem::Windows, Architecture::X86_64) => ("windows", "x86_64", ".exe"),
        _ => {
            return Err(invalid(
                "rolling desktop prebuilt updates are not published for this OS/architecture",
            ));
        }
    };
    Ok(format!(
        "medusa-desktop-main-{os}-{architecture}{extension}"
    ))
}

fn validate_revision(revision: &str) -> MedusaResult<()> {
    if !matches!(revision.len(), SHA1_HEX_LENGTH | SHA256_HEX_LENGTH)
        || !revision.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid("GitHub returned an invalid immutable revision"));
    }
    Ok(())
}

fn cargo_install_arguments(revision: &str, install_root: &Path) -> Vec<String> {
    vec![
        "install".to_owned(),
        "--git".to_owned(),
        REPOSITORY_URL.to_owned(),
        "--rev".to_owned(),
        revision.to_owned(),
        "--locked".to_owned(),
        "--bin".to_owned(),
        "medusa".to_owned(),
        "--root".to_owned(),
        install_root.to_string_lossy().into_owned(),
        "medusa-cli".to_owned(),
    ]
}

fn cargo_target_directory(repo: &Path) -> PathBuf {
    repo.join(".medusa")
        .join("update-cache")
        .join("cargo-target")
}

fn ensure_cargo_available() -> MedusaResult<()> {
    Command::new("cargo")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(command_error)
        .and_then(|status| {
            status.success().then_some(()).ok_or_else(|| {
                MedusaError::new(
                    ErrorCode::DependencyUnavailable,
                    ErrorCategory::Environment,
                    "cargo is required to update from Medusa main",
                )
            })
        })
}

fn run_cargo_install(
    revision: &str,
    install_root: &Path,
    target_dir: &Path,
    progress: &mut impl FnMut(MainBuildProgress),
) -> MedusaResult<usize> {
    let mut child = Command::new("cargo")
        .args(cargo_install_arguments(revision, install_root))
        .env("CARGO_TARGET_DIR", target_dir)
        .env("CARGO_TERM_COLOR", "never")
        .env("CARGO_TERM_PROGRESS_WHEN", "never")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(command_error)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| command_error("cargo stdout was not captured"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| command_error("cargo stderr was not captured"))?;
    let (sender, receiver) = mpsc::channel();
    let stdout_reader = spawn_line_reader(stdout, sender.clone());
    let stderr_reader = spawn_line_reader(stderr, sender);
    let started = Instant::now();
    let mut compiled = HashSet::new();
    let mut output_tail = VecDeque::with_capacity(8);
    progress(MainBuildProgress {
        compiled_packages: 0,
        current_package: None,
        elapsed: Duration::ZERO,
    });

    loop {
        match receiver.recv_timeout(Duration::from_millis(250)) {
            Ok(Ok(line)) => {
                let package = cargo_compiling_package(&line);
                if let Some(package_name) = package.as_ref() {
                    compiled.insert(package_name.clone());
                }
                if !line.trim().is_empty() {
                    if output_tail.len() == 8 {
                        output_tail.pop_front();
                    }
                    output_tail.push_back(line);
                }
                progress(MainBuildProgress {
                    compiled_packages: compiled.len(),
                    current_package: package,
                    elapsed: started.elapsed(),
                });
            }
            Ok(Err(error)) => {
                if output_tail.len() == 8 {
                    output_tail.pop_front();
                }
                output_tail.push_back(format!("cargo output error: {error}"));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => progress(MainBuildProgress {
                compiled_packages: compiled.len(),
                current_package: None,
                elapsed: started.elapsed(),
            }),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let status = child.wait().map_err(command_error)?;
    let _ = stdout_reader.join();
    let _ = stderr_reader.join();
    if !status.success() {
        let details = output_tail.into_iter().collect::<Vec<_>>().join("\n");
        return Err(build_error(status, details));
    }
    Ok(compiled.len())
}

fn spawn_line_reader<R>(
    reader: R,
    sender: mpsc::Sender<io::Result<String>>,
) -> thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            if sender.send(line).is_err() {
                break;
            }
        }
    })
}

fn cargo_compiling_package(line: &str) -> Option<String> {
    let line = line.trim_start();
    let package = line.strip_prefix("Compiling ")?;
    package.split_whitespace().next().map(str::to_owned)
}

fn medusa_binary_name() -> &'static str {
    if cfg!(windows) {
        "medusa.exe"
    } else {
        "medusa"
    }
}

fn build_error(status: std::process::ExitStatus, details: String) -> MedusaError {
    let suffix = if details.is_empty() {
        String::new()
    } else {
        format!(":\n{details}")
    };
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Environment,
        format!("cargo install failed with status {status}{suffix}"),
    )
}

fn command_error(error: impl std::fmt::Display) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Environment,
        format!("could not start the main-branch updater: {error}"),
    )
}

fn publish_timeout(revision: &str) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Transient,
        format!(
            "prebuilt main artifact for revision {revision} was not published within {} seconds",
            ROLLING_PUBLISH_WAIT.as_secs()
        ),
    )
    .with_retryable(true)
}

fn not_published(revision: &str) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Transient,
        format!("prebuilt main artifact for revision {revision} is not published yet"),
    )
    .with_retryable(true)
}

fn http_error(error: impl std::fmt::Display) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Transient,
        format!("GitHub main branch request failed: {error}"),
    )
    .with_retryable(true)
}

fn asset_error(error: impl std::fmt::Display) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Transient,
        format!("GitHub rolling main artifact request failed: {error}"),
    )
    .with_retryable(true)
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
        time::Duration,
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
    fn rolling_asset_selection_matches_published_platforms() {
        let linux = Platform {
            os: OperatingSystem::Linux,
            architecture: Architecture::X86_64,
        };
        assert_eq!(
            rolling_asset_name(linux, REVISION).expect("asset"),
            "medusa-main-linux-x86_64.tar.gz"
        );

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

        assert_eq!(
            rolling_desktop_asset_name(windows, REVISION).expect("desktop asset"),
            "medusa-desktop-main-windows-x86_64.exe"
        );
    }

    #[test]
    fn rolling_release_tag_is_revision_scoped() {
        assert_eq!(
            rolling_release_tag(REVISION).expect("release tag"),
            format!("main-{REVISION}")
        );
    }

    #[test]
    fn rolling_asset_requests_revision_scoped_release() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request");
            let mut request = [0_u8; 2048];
            let bytes = stream.read(&mut request).expect("read request");
            let request = String::from_utf8_lossy(&request[..bytes]);
            assert!(request.starts_with(&format!(
                "GET /main-{REVISION}/medusa-main-windows-x86_64.zip HTTP/1.1"
            )));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: 5\r\nConnection: close\r\n\r\nasset"
            )
            .expect("response");
        });
        let updater =
            MainBranchUpdater::with_asset_base("http://api.invalid", base).expect("client");
        let response = updater
            .asset_response_until(
                "medusa-main-windows-x86_64.zip",
                REVISION,
                Instant::now() + Duration::from_secs(1),
            )
            .expect("asset response");
        assert_eq!(response.status(), StatusCode::OK);
        worker.join().expect("server");
    }

    #[test]
    fn cli_artifact_status_requests_revision_scoped_manifest() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let asset_name = rolling_asset_name(Platform::current().expect("platform"), REVISION)
            .expect("asset name");
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request");
            let mut request = [0_u8; 2048];
            let bytes = stream.read(&mut request).expect("read request");
            let request = String::from_utf8_lossy(&request[..bytes]);
            assert!(
                request.starts_with(&format!("GET /main-{REVISION}/{asset_name}.json HTTP/1.1"))
            );
            stream
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("response");
        });
        let updater =
            MainBranchUpdater::with_asset_base("http://api.invalid", base).expect("client");
        assert!(
            !updater
                .main_cli_artifact_available(REVISION)
                .expect("artifact status")
        );
        worker.join().expect("server");
    }

    #[test]
    fn desktop_artifact_status_rejects_an_unpublished_revision_without_polling() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read request");
            stream
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("response");
        });
        let updater =
            MainBranchUpdater::with_asset_base("http://api.invalid", base).expect("client");
        assert!(
            !updater
                .artifact_manifest_available("medusa-desktop-main-windows-x86_64.exe", REVISION,)
                .expect("artifact status")
        );
        worker.join().expect("server");
    }

    #[test]
    fn rolling_asset_request_times_out_when_server_stalls() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request");
            thread::sleep(Duration::from_millis(100));
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        });
        let updater = MainBranchUpdater::with_asset_base_and_timeouts(
            "http://api.invalid",
            base,
            Duration::from_millis(25),
            Duration::from_millis(25),
        )
        .expect("client");

        let error = match updater.asset_response_once("manifest.json", REVISION) {
            Ok(_) => panic!("a stalled rolling asset request must time out"),
            Err(error) => error,
        };
        assert_eq!(error.code, ErrorCode::DependencyUnavailable);
        worker.join().expect("server");
    }

    #[test]
    fn rolling_asset_selection_rejects_unpublished_platforms_without_polling() {
        for platform in [
            Platform {
                os: OperatingSystem::Linux,
                architecture: Architecture::Aarch64,
            },
            Platform {
                os: OperatingSystem::Macos,
                architecture: Architecture::X86_64,
            },
            Platform {
                os: OperatingSystem::Windows,
                architecture: Architecture::Aarch64,
            },
        ] {
            let error = rolling_asset_name(platform, REVISION).expect_err("unsupported platform");
            assert_eq!(error.code, ErrorCode::InvalidConfiguration);
        }
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
        let error = manifest
            .validate(
                "1123456789abcdef0123456789abcdef01234567",
                "medusa-main-windows-x86_64.zip",
            )
            .expect_err("stale manifest");
        assert_eq!(error.code, ErrorCode::DependencyUnavailable);
        assert_eq!(error.category, ErrorCategory::Transient);
        assert!(error.retryable);
    }

    #[test]
    fn rolling_publish_timeout_is_retryable() {
        let error = publish_timeout(REVISION);
        assert_eq!(error.code, ErrorCode::DependencyUnavailable);
        assert_eq!(error.category, ErrorCategory::Transient);
        assert!(error.retryable);
    }

    #[test]
    fn source_build_command_is_revision_pinned_and_isolated() {
        let arguments = cargo_install_arguments(REVISION, Path::new(r"C:\tmp\medusa-build"));
        assert_eq!(arguments.first().map(String::as_str), Some("install"));
        assert!(
            arguments
                .windows(2)
                .any(|pair| { pair[0] == "--rev" && pair[1] == REVISION })
        );
        assert!(
            arguments
                .windows(2)
                .any(|pair| { pair[0] == "--root" && pair[1] == r"C:\tmp\medusa-build" })
        );
        assert!(!arguments.iter().any(|argument| argument == "--branch"));
    }

    #[test]
    fn source_build_target_directory_is_repo_scoped() {
        assert_eq!(
            cargo_target_directory(Path::new(r"C:\repo")),
            Path::new(r"C:\repo\.medusa\update-cache\cargo-target")
        );
    }

    #[test]
    fn cargo_compiling_lines_report_package_names() {
        assert_eq!(
            cargo_compiling_package("   Compiling medusa-core v1.0.0"),
            Some("medusa-core".to_owned())
        );
        assert_eq!(
            cargo_compiling_package("Compiling medusa-cli v1.0.4 (path+file:///tmp)"),
            Some("medusa-cli".to_owned())
        );
        assert_eq!(
            cargo_compiling_package("Finished release [optimized]"),
            None
        );
    }
}
