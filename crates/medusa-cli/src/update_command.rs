use std::{fs, io::IsTerminal, path::Path};

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

    match UpdateCheck::compare(env!("CARGO_PKG_VERSION"), release.version.clone()) {
        UpdateCheck::UpToDate { current } if !allow_downgrade => {
            println!(
                "Medusa {current} is current. Verified rollout sequence {} signed by {}.",
                release.rollout_sequence, release.signing_key_id
            );
            return Ok(());
        }
        UpdateCheck::Available { current, latest } => {
            println!(
                "Verified Medusa release available: {current} -> {latest} (sequence {}, key {}).",
                release.rollout_sequence, release.signing_key_id
            );
        }
        UpdateCheck::CurrentBuildUnparseable { current, latest } => {
            println!("Verified Medusa release available: {current} -> {latest}.");
        }
        UpdateCheck::UpToDate { current } => {
            println!(
                "Explicit rollback selected: {current} -> {} (sequence {}).",
                release.version, release.rollout_sequence
            );
        }
    }

    if !rollout_eligible(repo, release.rollout_percentage) {
        println!(
            "Release {} is in a {}% rollout and this installation is not selected yet.",
            release.version, release.rollout_percentage
        );
        return Ok(());
    }
    if check_only {
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

    println!(
        "Downloading {} bytes for {:?}/{:?}; the running session stays active until verification and staging finish.",
        artifact.bytes, platform.os, platform.architecture
    );
    let download_timer = diagnostics.phase(UpdatePhase::Download);
    let mut last_reported = 0_u64;
    let report = client.download(artifact, &archive, |downloaded, total| {
        let threshold = 4 * 1024 * 1024;
        if downloaded == total.unwrap_or(0) || downloaded.saturating_sub(last_reported) >= threshold {
            last_reported = downloaded;
            if let Some(total) = total {
                eprintln!("update download: {downloaded}/{total} bytes");
            }
        }
    })?;
    download_timer.finish("downloaded-and-verified", Some(report.bytes), Some(report.retries))?;
    diagnostics
        .phase(UpdatePhase::ArtifactVerification)
        .finish("sha256-and-size-verified", Some(report.bytes), None)?;

    let installer = AtomicInstaller::new(location.executable.clone());
    let extraction_timer = diagnostics.phase(UpdatePhase::Extraction);
    let candidate = installer.extract_archive(&archive, &workspace.path().join("extract"))?;
    extraction_timer.finish("confined", None, None)?;

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
    let scheduled = installer.schedule_replace(&candidate, &restart, std::process::id())?;
    staging_timer.finish("atomic-handoff-staged", Some(artifact.bytes), None)?;
    super::request_daemon_shutdown(repo);
    diagnostics
        .phase(UpdatePhase::RestartHandoff)
        .finish("health-check-pending", None, None)?;
    println!(
        "Verified release {} is staged. After this process exits, Medusa will restart the session, require a health handshake, and roll back automatically on failure. State: {}",
        release.version,
        scheduled.state.display()
    );
    Ok(())
}

fn source_channel(repo: &Path, check_only: bool, automatic: bool) -> MedusaResult<()> {
    let policy = UpdatePolicy::from_environment();
    let check_only = check_only || policy == UpdatePolicy::Check;
    let automatic = automatic || policy == UpdatePolicy::Automatic;
    eprintln!(
        "Updating from the latest Medusa main branch. This path invokes Cargo and compiles locally; use `medusa update --release` for a verified prebuilt release."
    );
    let updater = MainBranchUpdater::public()?;
    let latest = updater.latest_main()?;
    let current = env!("MEDUSA_BUILD_COMMIT");
    if current == latest.sha {
        println!("Medusa is already running main commit {current}.");
        return Ok(());
    }
    println!("Medusa main update available: {current} -> {}", latest.sha);
    if check_only {
        return Ok(());
    }
    require_automatic_for_unattended(automatic)?;
    let location = InstallLocation::current()?;
    if let InstallKind::PackageManaged { manager, command } = location.kind {
        println!("This Medusa binary is managed by {manager}. Update it with: {command}");
        return Ok(());
    }
    super::request_daemon_shutdown(repo);
    updater.schedule_main_install(&location.executable, std::process::id())?;
    println!("The latest main-branch source build is scheduled after this process exits.");
    Ok(())
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
}
