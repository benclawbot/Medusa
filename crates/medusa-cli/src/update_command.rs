use std::{
    fs,
    io::{self, IsTerminal, Write},
    path::Path,
    time::{Duration, Instant},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_update::{
    AtomicInstaller, GithubReleaseClient, InstallKind, InstallLocation, MainArtifactPhase,
    MainArtifactProgress, MainBranchUpdater, MainBuildProgress, Platform, ReleaseClient, Restart,
    CURRENT_RELEASE_ID, ReleaseId, UpdateDiagnostics, UpdatePhase, UpdatePolicy,
};
use crossterm::terminal;
use semver::Version;
use sha2::{Digest, Sha256};

const BUILD_PHASE_START: u8 = 5;
const BUILD_PHASE_END: u8 = 78;
// Fallback when the exact revision's Cargo metadata cannot be resolved before
// the build starts. Normal builds display the resolved package count instead.
const BUILD_ESTIMATE: Duration = Duration::from_secs(360);
const BUILD_PIECE_ESTIMATE: usize = 272;
const PROGRESS_WIDTH: usize = 32;
const MIN_PROGRESS_BAR_WIDTH: usize = 12;
const DEFAULT_TERMINAL_WIDTH: usize = 120;

// Default time to wait for a CI-built prebuilt main artifact before either
// falling back to a local compile (`--local-build`) or refusing the update.
const DEFAULT_PREBUILT_WAIT_SECS: u64 = 600;
// How often to poll the release endpoint while waiting for the prebuilt
// artifact. The CI publish step typically finishes in under five minutes.
const PREBUILT_POLL_INTERVAL_SECS: u64 = 15;

pub(super) fn run(
    repo: &Path,
    check_only: bool,
    automatic: bool,
    release: bool,
    allow_downgrade: bool,
    local_build: bool,
    wait_for_prebuilt: Option<u64>,
) -> MedusaResult<()> {
    if release {
        release_channel(repo, check_only, automatic, allow_downgrade)
    } else {
        source_channel(repo, check_only, automatic, local_build, wait_for_prebuilt)
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
    let current_release_id = ReleaseId::parse(CURRENT_RELEASE_ID)
        .map_err(|error| invalid(format!("invalid current release identity: {error}")))?;
    if !allow_downgrade && release.release_id < current_release_id {
        return Err(policy_error(format!(
            "downgrade from {} to {} requires --allow-downgrade",
            current_release_id, release.release_id
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

    if release.release_id <= current_release_id && !allow_downgrade {
        println!("Medusa is up to date.");
        return Ok(());
    }

    if !rollout_eligible(repo, release.rollout_percentage) {
        println!(
            "Release {} is in a {}% rollout and this installation is not selected yet.",
            release.release_id, release.rollout_percentage
        );
        return Ok(());
    }
    if check_only {
        println!("Verified Medusa release {} is available.", release.release_id);
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
    let mut progress = UpdateProgress::new(
        current_release_id.to_string(),
        release.release_id.to_string(),
    );
    progress.stage(UpdateStage::Preparing, 0, "selecting release");

    let download_timer = diagnostics.phase(UpdatePhase::Download);
    let report = client.download(artifact, &archive, |downloaded, total| {
        progress.download(downloaded, total, 4, 88);
    })?;
    progress.stage(UpdateStage::Verifying, 90, "checksum verified");
    download_timer.finish("downloaded-and-verified", Some(report.bytes), Some(report.retries))?;
    diagnostics
        .phase(UpdatePhase::ArtifactVerification)
        .finish("sha256-and-size-verified", Some(report.bytes), None)?;

    let installer = AtomicInstaller::new(location.executable.clone());
    let extraction_timer = diagnostics.phase(UpdatePhase::Extraction);
    let candidate = installer.extract_archive(&archive, &workspace.path().join("extract"))?;
    extraction_timer.finish("confined", None, None)?;
    progress.stage(UpdateStage::Installing, 95, "archive extracted");

    let staging_timer = diagnostics.phase(UpdatePhase::Staging);
    let restart = Restart {
        arguments: vec![
            "--repo".to_owned(),
            repo.to_string_lossy().into_owned(),
            "--continue".to_owned(),
        ],
        detached: cfg!(windows),
        sequence_file: Some(repo.join(".medusa/update-sequence")),
        rollout_sequence: Some(release.rollout_sequence),
    };
    super::request_daemon_shutdown(repo);
    installer.schedule_replace(&candidate, &restart, std::process::id())?;
    staging_timer.finish("atomic-handoff-staged", Some(artifact.bytes), None)?;

    #[cfg(windows)]
    {
        progress.stage(UpdateStage::Installing, 99, "replacing executable");
        wait_for_windows_replacement();
    }

    #[cfg(not(windows))]
    {
        progress.stage(UpdateStage::Installing, 98, "atomic restart staged");
        diagnostics
            .phase(UpdatePhase::RestartHandoff)
            .finish("health-check-pending", None, None)?;
        progress.finish();
        println!(
            "Medusa update installed and staged: {}. Restarting.",
            version_transition(
                &current_release_id.to_string(),
                &release.release_id.to_string(),
            )
        );
        Ok(())
    }
}

fn source_channel(
    repo: &Path,
    check_only: bool,
    automatic: bool,
    local_build: bool,
    wait_for_prebuilt: Option<u64>,
) -> MedusaResult<()> {
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

    let current_version = main_revision_label(current);
    let new_version = main_revision_label(&latest.sha);
    let mut progress = UpdateProgress::new(current_version.clone(), new_version.clone());
    progress.stage(UpdateStage::Preparing, 0, "checking prebuilt main artifact");
    let strategy = main_update_strategy(
        &updater,
        &latest.sha,
        local_build,
        wait_for_prebuilt,
        &mut progress,
    )?;
    super::request_daemon_shutdown(repo);
    match strategy {
        MainUpdateStrategy::Prebuilt => progress.stage(
            UpdateStage::Preparing,
            2,
            "prebuilt main artifact ready",
        ),
        MainUpdateStrategy::LocalBuild => progress.stage(
            UpdateStage::Building,
            BUILD_PHASE_START,
            "prebuilt artifact pending · compiling locally",
        ),
    }
    match strategy {
        MainUpdateStrategy::Prebuilt => updater.schedule_main_install_with_progress(
            &location.executable,
            repo,
            std::process::id(),
            |snapshot| progress.artifact(snapshot),
        )?,
        MainUpdateStrategy::LocalBuild => updater.build_and_schedule_main_install(
            &location.executable,
            repo,
            std::process::id(),
            |snapshot| progress.build(snapshot),
        )?,
    };

    #[cfg(windows)]
    {
        progress.stage(UpdateStage::Installing, 99, "replacing executable");
        wait_for_windows_replacement();
    }

    #[cfg(not(windows))]
    {
        progress.stage(UpdateStage::Installing, 98, "atomic restart staged");
        progress.finish();
        let update_kind = match strategy {
            MainUpdateStrategy::Prebuilt => "downloaded",
            MainUpdateStrategy::LocalBuild => "built",
        };
        println!(
            "Medusa update {} and staged: {}. Restarting.",
            update_kind,
            version_transition(&current_version, &new_version),
        );
        Ok(())
    }
}

fn short_revision(revision: &str) -> &str {
    revision.get(..12).unwrap_or(revision)
}

fn main_revision_label(revision: &str) -> String {
    format!("main ({})", short_revision(revision))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MainUpdateStrategy {
    Prebuilt,
    LocalBuild,
}

fn main_update_strategy(
    updater: &MainBranchUpdater,
    revision: &str,
    local_build: bool,
    wait_for_prebuilt: Option<u64>,
    progress: &mut UpdateProgress,
) -> MedusaResult<MainUpdateStrategy> {
    if updater.main_cli_artifact_available(revision)? {
        return Ok(MainUpdateStrategy::Prebuilt);
    }
    if local_build {
        return Ok(MainUpdateStrategy::LocalBuild);
    }
    let timeout_secs = wait_for_prebuilt
        .or_else(|| {
            std::env::var("MEDUSA_UPDATE_PREBUILT_TIMEOUT_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(DEFAULT_PREBUILT_WAIT_SECS);
    let poll_interval = Duration::from_secs(PREBUILT_POLL_INTERVAL_SECS);
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut elapsed = Duration::ZERO;
    let short_rev = short_revision(revision);
    loop {
        progress.stage(
            UpdateStage::Waiting,
            1,
            format!(
                "waiting for CI prebuilt main artifact for {short_rev} (elapsed {}s of {timeout_secs}s)",
                elapsed.as_secs()
            ),
        );
        if Instant::now() >= deadline {
            return Err(MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Transient,
                format!(
                    "no prebuilt main artifact for {short_rev} after waiting {timeout_secs}s. \
                     Either wait longer with `--wait-for-prebuilt=<secs>`, opt out with \
                     `--local-build` to compile from source (~15 minutes), or check the \
                     `rolling-main-cli` GitHub Actions workflow for failures."
                ),
            )
            .with_retryable(true));
        }
        std::thread::sleep(poll_interval);
        let started = deadline
            .checked_sub(Duration::from_secs(timeout_secs))
            .unwrap_or(deadline);
        elapsed = Instant::now().saturating_duration_since(started);
        if updater.main_cli_artifact_available(revision)? {
            return Ok(MainUpdateStrategy::Prebuilt);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UpdateStage {
    Preparing,
    Waiting,
    Building,
    Downloading,
    Verifying,
    Installing,
    #[cfg(not(windows))]
    Complete,
}

impl UpdateStage {
    fn label(self) -> &'static str {
        match self {
            Self::Preparing => "Preparing",
            Self::Waiting => "Waiting",
            Self::Building => "Building",
            Self::Downloading => "Downloading",
            Self::Verifying => "Verifying",
            Self::Installing => "Installing",
            #[cfg(not(windows))]
            Self::Complete => "Complete",
        }
    }

    fn color(self) -> &'static str {
        match self {
            Self::Preparing => "\u{1b}[36m",
            Self::Waiting => "\u{1b}[90m",
            Self::Building => "\u{1b}[33m",
            Self::Downloading => "\u{1b}[34m",
            Self::Verifying => "\u{1b}[35m",
            Self::Installing => "\u{1b}[32m",
            #[cfg(not(windows))]
            Self::Complete => "\u{1b}[1;32m",
        }
    }
}

struct UpdateProgress {
    enabled: bool,
    colors: bool,
    started: bool,
    finished: bool,
    last_percent: u8,
    stage: UpdateStage,
    detail: String,
    stage_label: String,
    current_version: String,
    new_version: String,
    download_started: Option<Instant>,
    terminal_width: usize,
}

impl UpdateProgress {
    fn new(current_version: String, new_version: String) -> Self {
        let enabled = io::stderr().is_terminal();
        Self {
            enabled,
            colors: enabled && std::env::var_os("NO_COLOR").is_none(),
            started: false,
            finished: false,
            last_percent: u8::MAX,
            stage: UpdateStage::Preparing,
            detail: String::new(),
            stage_label: UpdateStage::Preparing.label().to_owned(),
            current_version,
            new_version,
            download_started: None,
            terminal_width: terminal_width(),
        }
    }

    fn download(&mut self, downloaded: u64, total: Option<u64>, start: u8, end: u8) {
        let started = *self.download_started.get_or_insert_with(Instant::now);
        let elapsed = started.elapsed();
        let detail = download_detail(downloaded, total, elapsed);
        let Some(total) = total.filter(|total| *total > 0) else {
            self.stage(UpdateStage::Downloading, start, detail);
            return;
        };
        let span = u64::from(end.saturating_sub(start));
        let fraction = downloaded.min(total).saturating_mul(span) / total;
        self.stage(
            UpdateStage::Downloading,
            start.saturating_add(fraction as u8),
            detail,
        );
    }

    fn build(&mut self, snapshot: MainBuildProgress) {
        let total = snapshot
            .total_packages
            .filter(|total| *total > 0)
            .unwrap_or(BUILD_PIECE_ESTIMATE);
        let total_prefix = if snapshot.total_packages.is_some() {
            ""
        } else {
            "~"
        };
        let package = snapshot
            .current_package
            .as_deref()
            .map(|package| format!(" · {package}"))
            .unwrap_or_default();
        let phase = if snapshot.compiled_packages == 0 {
            "resolving dependencies · ".to_owned()
        } else {
            String::new()
        };
        self.stage_with_label(
            UpdateStage::Building,
            estimate_build_percent(snapshot.elapsed, snapshot.compiled_packages, total),
            format!(
                "{}{} elapsed{}",
                phase,
                format_elapsed(snapshot.elapsed),
                package
            ),
            format!(
                "Building {}/{}{} crates",
                snapshot.compiled_packages, total_prefix, total
            ),
        );
    }

    fn artifact(&mut self, snapshot: MainArtifactProgress) {
        match snapshot.phase {
            MainArtifactPhase::Waiting => self.stage(
                UpdateStage::Waiting,
                2,
                format!(
                    "waiting for prebuilt main artifact · {} elapsed",
                    format_elapsed(snapshot.elapsed)
                ),
            ),
            MainArtifactPhase::Downloading => {
                self.download(snapshot.downloaded, snapshot.total, 4, 88)
            }
            MainArtifactPhase::Verifying => self.stage(
                UpdateStage::Verifying,
                90,
                "verifying manifest and SHA-256",
            ),
        }
    }

    fn stage(&mut self, stage: UpdateStage, percent: u8, detail: impl Into<String>) {
        self.stage_with_label(stage, percent, detail, stage.label());
    }

    fn stage_with_label(
        &mut self,
        stage: UpdateStage,
        percent: u8,
        detail: impl Into<String>,
        stage_label: impl Into<String>,
    ) {
        if !self.enabled {
            return;
        }
        let percent = percent.min(100);
        let detail = detail.into();
        let stage_label = stage_label.into();
        if self.last_percent == percent
            && self.stage == stage
            && self.detail == detail
            && self.stage_label == stage_label
        {
            return;
        }
        self.started = true;
        self.last_percent = percent;
        self.stage = stage;
        self.detail = detail;
        self.stage_label = stage_label;
        let mut stderr = io::stderr().lock();
        let _ = write!(
            stderr,
            "{}",
            render_progress_line_with_width(
                ProgressLine {
                    stage: self.stage,
                    stage_label: &self.stage_label,
                    percent,
                    detail: &self.detail,
                    current_version: &self.current_version,
                    new_version: &self.new_version,
                    colors: self.colors,
                },
                self.terminal_width,
            )
        );
        let _ = stderr.flush();
    }

    #[cfg(not(windows))]
    fn finish(&mut self) {
        self.stage(UpdateStage::Complete, 100, "ready");
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

#[cfg(windows)]
fn wait_for_windows_replacement() -> ! {
    loop {
        std::thread::sleep(Duration::from_secs(60));
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

fn estimate_build_percent(elapsed: Duration, compiled_packages: usize, total_packages: usize) -> u8 {
    let time_ratio = elapsed.as_secs_f64() / BUILD_ESTIMATE.as_secs_f64();
    let pieces_ratio = compiled_packages as f64 / total_packages.max(1) as f64;
    let ratio = (time_ratio * 0.65 + pieces_ratio * 0.35).clamp(0.0, 0.98);
    let span = f64::from(BUILD_PHASE_END - BUILD_PHASE_START);
    BUILD_PHASE_START.saturating_add((ratio * span).round() as u8)
}

struct ProgressLine<'a> {
    stage: UpdateStage,
    stage_label: &'a str,
    percent: u8,
    detail: &'a str,
    current_version: &'a str,
    new_version: &'a str,
    colors: bool,
}

#[cfg(test)]
fn render_progress_line(
    stage: UpdateStage,
    percent: u8,
    detail: &str,
    current_version: &str,
    new_version: &str,
    colors: bool,
) -> String {
    render_progress_line_with_width(
        ProgressLine {
            stage,
            stage_label: stage.label(),
            percent,
            detail,
            current_version,
            new_version,
            colors,
        },
        usize::MAX,
    )
}

fn render_progress_line_with_width(line: ProgressLine<'_>, terminal_width: usize) -> String {
    let ProgressLine {
        stage,
        stage_label,
        percent,
        detail,
        current_version,
        new_version,
        colors,
    } = line;
    let percent = percent.min(100);
    let percent_label = if stage == UpdateStage::Building {
        format!("{percent:3}% est.")
    } else {
        format!("{percent:3}%")
    };
    let title_prefix = "Updating Medusa ";
    let suffix = format!("] {percent_label} · {stage_label} · ");
    let minimum_bar_width = MIN_PROGRESS_BAR_WIDTH.min(terminal_width);
    let title_budget = terminal_width
        .saturating_sub(title_prefix.chars().count())
        .saturating_sub(suffix.chars().count())
        .saturating_sub(minimum_bar_width)
        .saturating_sub(2);
    let title = truncate_display(&version_transition(current_version, new_version), title_budget);
    let overhead = title_prefix.chars().count()
        + title.chars().count()
        + 2
        + suffix.chars().count();
    let bar_width = PROGRESS_WIDTH.min(
        terminal_width
            .saturating_sub(overhead)
            .max(minimum_bar_width),
    );
    let detail_budget = terminal_width.saturating_sub(overhead + bar_width);
    let detail = truncate_display(detail, detail_budget);
    let filled = usize::from(percent) * bar_width / 100;
    let plain_bar = format!(
        "{}{}",
        "█".repeat(filled),
        "░".repeat(bar_width.saturating_sub(filled))
    );
    let bar = if colors {
        format!(
            "{}{}\u{1b}[0m\u{1b}[2m{}\u{1b}[0m",
            stage.color(),
            "█".repeat(filled),
            "░".repeat(bar_width.saturating_sub(filled))
        )
    } else {
        plain_bar.clone()
    };
    let prefix = if colors { "\r\u{1b}[2K" } else { "\r" };
    let visible_line = format!(
        "{title_prefix}{title} [{plain_bar}] {percent_label} · {stage_label} · {detail}"
    );
    debug_assert!(
        terminal_width == usize::MAX || visible_line.chars().count() <= terminal_width,
        "progress line exceeds terminal width: {} > {}",
        visible_line.chars().count(),
        terminal_width
    );
    if colors {
        format!(
            "{prefix}\u{1b}[1;36mUpdating Medusa\u{1b}[0m {title} [{bar}] {percent_label} · {stage_label} · {detail}"
        )
    } else {
        format!("{prefix}{visible_line}")
    }
}

fn terminal_width() -> usize {
    terminal::size()
        .ok()
        .map(|(width, _)| usize::from(width))
        .filter(|width| *width > 0)
        .unwrap_or(DEFAULT_TERMINAL_WIDTH)
}

fn truncate_display(value: &str, max_width: usize) -> String {
    let width = value.chars().count();
    if width <= max_width {
        return value.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_owned();
    }
    let mut truncated = value.chars().take(max_width - 1).collect::<String>();
    truncated.push('…');
    truncated
}

fn version_transition(current: &str, new: &str) -> String {
    format!("{current} → {new}")
}

fn download_detail(downloaded: u64, total: Option<u64>, elapsed: Duration) -> String {
    let seconds = elapsed.as_secs_f64().max(0.001);
    let bytes_per_second = downloaded as f64 / seconds;
    let rate = format_rate(bytes_per_second);
    match total.filter(|total| *total > 0) {
        Some(total) => {
            let remaining = total.saturating_sub(downloaded);
            let eta_seconds = remaining as f64 / bytes_per_second.max(0.001);
            let eta = if bytes_per_second <= 0.0 {
                String::new()
            } else {
                format!(
                    " · ETA {}",
                    format_elapsed(Duration::from_secs_f64(
                        eta_seconds.min(99.0 * 60.0 * 60.0),
                    ))
                )
            };
            format!(
                "{} / {} · {}{}",
                format_bytes(downloaded),
                format_bytes(total),
                rate,
                eta
            )
        }
        None => format!("{} · {}", format_bytes(downloaded), rate),
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn format_rate(bytes_per_second: f64) -> String {
    if bytes_per_second <= 0.0 {
        return String::new();
    }
    format!("{}/s", format_bytes(bytes_per_second as u64))
}

fn format_elapsed(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
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

    #[test]
    fn build_progress_estimate_uses_elapsed_time_and_compiled_pieces() {
        let early = estimate_build_percent(Duration::from_secs(5), 2, BUILD_PIECE_ESTIMATE);
        let more_pieces = estimate_build_percent(Duration::from_secs(5), 12, BUILD_PIECE_ESTIMATE);
        let more_time = estimate_build_percent(Duration::from_secs(30), 2, BUILD_PIECE_ESTIMATE);

        assert!(more_pieces > early);
        assert!(more_time > early);
        assert!(more_time < BUILD_PHASE_END);
    }

    #[test]
    fn build_progress_line_shows_compiled_and_total_crates() {
        let line = render_progress_line_with_width(
            ProgressLine {
                stage: UpdateStage::Building,
                stage_label: "Building 235/305 crates",
                percent: 77,
                detail: "02:10 elapsed · medusa-runtime",
                current_version: "1.0.6 (old)",
                new_version: "1.0.7 (new)",
                colors: false,
            },
            120,
        );

        assert!(line.contains("Building 235/305 crates"));
        assert!(line.contains("77% est."));
    }

    #[test]
    fn colored_progress_line_includes_phase_detail_and_versions() {
        let line = render_progress_line(
            UpdateStage::Building,
            42,
            "12 crates · 20s",
            "1.0.4 (old)",
            "1.0.4 (new)",
            true,
        );

        assert!(line.contains("Building"));
        assert!(line.contains("12 crates · 20s"));
        assert!(line.contains("42%"));
        assert!(line.contains("1.0.4 (old) → 1.0.4 (new)"));
        assert!(line.contains("\u{1b}["));
    }

    #[test]
    fn plain_progress_line_omits_terminal_escape_sequences() {
        let line = render_progress_line(
            UpdateStage::Installing,
            92,
            "staging atomic restart",
            "old",
            "new",
            false,
        );

        assert!(line.contains("Installing"));
        assert!(!line.contains("\u{1b}["));
    }

    #[test]
    fn progress_line_is_width_safe_for_narrow_windows_terminals() {
        let line = render_progress_line_with_width(
            ProgressLine {
                stage: UpdateStage::Downloading,
                stage_label: "Downloading",
                percent: 42,
                detail: "123.4 MiB / 567.8 MiB · 12.4 MiB/s · ETA 00:37",
                current_version: "1.0.5 (5b97a73ef0d4)",
                new_version: "1.0.5 (5c17d7f00f4f)",
                colors: false,
            },
            80,
        );
        let visible = line.trim_start_matches('\r');
        assert!(visible.chars().count() <= 80, "line wrapped: {visible}");
    }

    #[test]
    fn version_transition_is_explicit() {
        assert_eq!(
            version_transition("1.0.4 (old)", "1.0.4 (new)"),
            "1.0.4 (old) → 1.0.4 (new)"
        );
    }

    #[test]
    fn main_revision_labels_use_the_revision_instead_of_package_version() {
        assert_eq!(
            main_revision_label("f95e04f9bfb5deadbeef"),
            "main (f95e04f9bfb5)"
        );
    }

    #[test]
    fn main_updates_default_to_waiting_when_prebuilt_missing() {
        assert!(std::hint::black_box(DEFAULT_PREBUILT_WAIT_SECS) >= 60);
        assert!(std::hint::black_box(PREBUILT_POLL_INTERVAL_SECS) >= 5);
    }

    #[test]
    fn waiting_progress_line_identifies_artifact_build_phase() {
        let line = render_progress_line(
            UpdateStage::Waiting,
            2,
            "waiting for prebuilt main artifact",
            "1.0.4 (old)",
            "1.0.4 (new)",
            false,
        );

        assert!(line.contains("Waiting"));
        assert!(line.contains("prebuilt main artifact"));
    }

    #[test]
    fn download_detail_reports_rate_and_remaining_time() {
        let detail = download_detail(512 * 1024, Some(1024 * 1024), Duration::from_secs(2));

        assert!(detail.contains("512.0 KiB"));
        assert!(detail.contains("256.0 KiB/s"));
        assert!(detail.contains("ETA 00:02"));
    }
}
