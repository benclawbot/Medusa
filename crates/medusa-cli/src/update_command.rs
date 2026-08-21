use std::{
    fs,
    io::{self, IsTerminal, Write},
    path::Path,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_update::{
    AtomicInstaller, GithubReleaseClient, InstallKind, InstallLocation, MainBranchUpdater, Platform,
    ReleaseClient, Restart, UpdateCheck, UpdateDiagnostics, UpdatePhase, UpdatePolicy,
};
use semver::Version;
use sha2::{Digest, Sha256};

pub(super) fn run(
    repo: &Path,
    check_only: bool,
    automatic: bool,
    release: bool,
    allow_downgrade: bool,
) -> MedusaResult<()> {
    if release {
        release_channel(repo, check_only, automatic, allow_downgrade)
    } else {
        source_channel(repo, check_only, automatic)
    }
}

fn release_channel(
    repo: &Path,
    check_only: bool,
    automatic: bool,
    allow_downgrade: bool,
) -> MedusaResult<()> {
    let policy = UpdatePolicy::from_environment();
    let check_only = check_only || policy == UpdatePolicy::Check;
    let automatic = automatic || policy == UpdatePolicy::Automatic;
    let diagnostics = UpdateDiagnostics::for_repository(repo);
    let metadata_timer = diagnostics.phase(UpdatePhase::Check);
    let client = GithubReleaseClient::public()?.with_cache_dir(repo.join(".medusa/update-cache"));
    let Some(release) = client.latest()? else {
        metadata_timer.finish("no-stable-release", None, None)?;
        println!("No stable verified Medusa release has been published yet.");
        return Ok(());
    };
    metadata_timer.finish("manifest-verified", None, None)?;
    diagnostics
        .phase(UpdatePhase::ManifestVerification)
        .finish("ed25519-verified", None, None)?;

    let current = Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|error| invalid(format!("invalid running version: {error}")))?;
    let updater = current.clone();
    let location = InstallLocation::current()?;
    let installed_sequence = read_installed_sequence(repo);
    if updater < release.minimum_updater_version {
        return Err(invalid(format!(
            "release {} requires updater {}, but this binary is {}",
            release.version, release.minimum_updater_version, updater
        )));
    }
    if !allow_downgrade && release.version < current {
        return Err(policy_error(format!(
            "downgrade from {current} to {} requires --allow-downgrade",
            release.version
        )));
    }
    if !allow_downgrade
        && installed_sequence.is_some_and(|sequence| release.rollout_sequence < sequence)
    {
        return Err(policy_error(format!(
            "release sequence {} is older than installed sequence {}; use --allow-downgrade only for an intentional rollback",
            release.rollout_sequence,
            installed_sequence.unwrap_or_default()
        )));
    }

    if matches!(
        UpdateCheck::compare(env!("CARGO_PKG_VERSION"), release.version.clone()),
        UpdateCheck::UpToDate { .. }
    ) && !allow_downgrade
    {
        println!("Medusa is up to date.");
        return Ok(());
    }

    if !rollout_eligible(repo, release.rollout_percentage) {
        println!(
            "Release {} is in a {}% rollout and this installation is not selected yet.",
            release.version, release.rollout_percentage
        );
        return Ok(());
    }
    if check_only {
        println!("Verified Medusa release {} is available.", release.version);
        return Ok(());
    }
    require_automatic_for_unattended(automatic)?;

    if let InstallKind::PackageManaged { manager, command } = location.kind {
        println!(
            "This Medusa binary is managed by {manager}. The verified updater will not invoke a package manager; update it explicitly with: {command}"
        );
        return Ok(());
    }

    let platform = Platform::current()
        .map_err(|error| invalid(format!("cannot select release artifact: {error}")))?;
    let artifact = release.artifact_for(&platform)?;
    let update_root = repo.join(".medusa/update-work");
    fs::create_dir_all(&update_root)?;
    let workspace = tempfile::Builder::new()
        .prefix("verified-release-")
        .tempdir_in(&update_root)?;
    let archive = workspace.path().join(&artifact.name);
    let mut progress = UpdateProgress::new();
    progress.set(2);

    let download_timer = diagnostics.phase(UpdatePhase::Download);
    let report = client.download(artifact, &archive, |downloaded, total| {
        progress.download(downloaded, total, 4, 88);
    })?;
    progress.set(90);
    download_timer.finish("downloaded-and-verified", Some(report.bytes), Some(report.retries))?;
    diagnostics
        .phase(UpdatePhase::ArtifactVerification)
        .finish("sha256-and-size-verified", Some(report.bytes), None)?;

    let installer = AtomicInstaller::new(location.executable.clone());
    let extraction_timer = diagnostics.phase(UpdatePhase::Extraction);
    let candidate = installer.extract_archive(&archive, &workspace.path().join("extract"))?;
    extraction_timer.finish("confined", None, None)?;
    progress.set(95);

    let staging_timer = diagnostics.phase(UpdatePhase::Staging);
    let restart = Restart {
        arguments: vec![
            "--repo".to_owned(),
            repo.to_string_lossy().into_owned(),
            "--continue".to_owned(),
        ],
        sequence_file: Some(repo.join(".medusa/update-sequence")),
        rollout_sequence: Some(release.rollout_sequence),
    };
    installer.schedule_replace(&candidate, &restart, std::process::id())?;
    staging_timer.finish("atomic-handoff-staged", Some(artifact.bytes), None)?;
    progress.set(98);
    super::request_daemon_shutdown(repo);
    diagnostics
        .phase(UpdatePhase::RestartHandoff)
        .finish("health-check-pending", None, None)?;
    progress.finish();
    Ok(())
}

fn source_channel(repo: &Path, check_only: bool, automatic: bool) -> MedusaResult<()> {
    let policy = UpdatePolicy::from_environment();
    let check_only = check_only || policy == UpdatePolicy::Check;
    let automatic = automatic || policy == UpdatePolicy::Automatic;
    let updater = MainBranchUpdater::public()?;
    let latest = updater.latest_main()?;
    let current = env!("MEDUSA_BUILD_COMMIT");
    if current == latest.sha {
        println!("Medusa is up to date.");
        return Ok(());
    }
    if check_only {
        println!(
            "Medusa main update available: {} -> {}.",
            short_revision(current),
            short_revision(&latest.sha)
        );
        return Ok(());
    }
    require_automatic_for_unattended(automatic)?;
    let location = InstallLocation::current()?;
    if let InstallKind::PackageManaged { manager, command } = location.kind {
        println!("This Medusa binary is managed by {manager}. Update it with: {command}");
        return Ok(());
    }

    let mut progress = UpdateProgress::new();
    progress.set(2);
    updater.schedule_main_install(
        &location.executable,
        repo,
        std::process::id(),
        |downloaded, total| progress.download(downloaded, total, 4, 92),
    )?;
    progress.set(98);
    super::request_daemon_shutdown(repo);
    progress.finish();
    Ok(())
}

fn short_revision(revision: &str) -> &str {
    revision.get(..12).unwrap_or(revision)
}

struct UpdateProgress {
    enabled: bool,
    started: bool,
    finished: bool,
    last_percent: u8,
}

impl UpdateProgress {
    fn new() -> Self {
        Self {
            enabled: io::stderr().is_terminal(),
            started: false,
            finished: false,
            last_percent: u8::MAX,
        }
    }

    fn download(&mut self, downloaded: u64, total: Option<u64>, start: u8, end: u8) {
        let Some(total) = total.filter(|total| *total > 0) else {
            self.set(start);
            return;
        };
        let span = u64::from(end.saturating_sub(start));
        let fraction = downloaded.min(total).saturating_mul(span) / total;
        self.set(start.saturating_add(fraction as u8));
    }

    fn set(&mut self, percent: u8) {
        if !self.enabled {
            return;
        }
        let percent = percent.min(100);
        if self.last_percent == percent {
            return;
        }
        self.started = true;
        self.last_percent = percent;
        const WIDTH: usize = 28;
        let filled = usize::from(percent) * WIDTH / 100;
        let mut stderr = io::stderr().lock();
        let _ = write!(
            stderr,
            "\rUpdating Medusa [{}{}] {percent:3}%",
            "=".repeat(filled),
            " ".repeat(WIDTH.saturating_sub(filled))
        );
        let _ = stderr.flush();
    }

    fn finish(&mut self) {
        self.set(100);
        if self.enabled {
            eprintln!();
        }
        self.finished = true;
    }
}

impl Drop for UpdateProgress {
    fn drop(&mut self) {
        if self.enabled && self.started && !self.finished {
            eprintln!();
        }
    }
}

fn require_automatic_for_unattended(automatic: bool) -> MedusaResult<()> {
    if automatic || std::io::stdin().is_terminal() {
        Ok(())
    } else {
        Err(policy_error(
            "refusing unattended replacement; use medusa update --automatic",
        ))
    }
}

fn rollout_eligible(repo: &Path, percentage: u8) -> bool {
    if percentage >= 100 {
        return true;
    }
    let digest = Sha256::digest(repo.to_string_lossy().as_bytes());
    let cohort = u16::from_be_bytes([digest[0], digest[1]]) % 100;
    cohort < u16::from(percentage)
}

fn read_installed_sequence(repo: &Path) -> Option<u64> {
    fs::read_to_string(repo.join(".medusa/update-sequence"))
        .ok()
        .and_then(|value| value.trim().parse().ok())
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

fn policy_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::PolicyDenied, ErrorCategory::Policy, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollout_is_stable_and_bounded() {
        let repo = Path::new("/stable/repository");
        assert_eq!(rollout_eligible(repo, 50), rollout_eligible(repo, 50));
        assert!(rollout_eligible(repo, 100));
    }

    #[test]
    fn short_revision_is_bounded() {
        assert_eq!(short_revision("0123456789abcdef"), "0123456789ab");
        assert_eq!(short_revision("short"), "short");
    }
}
