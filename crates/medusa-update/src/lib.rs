//! Verified prebuilt-release self-update primitives.
//!
//! The CLI owns user interaction; this crate owns signed release discovery,
//! exact artifact verification, confined extraction, diagnostics, and a
//! health-checked atomic installation with rollback.

mod diagnostics;
mod github;
// Windows keeps this module compiled for the shared archive/recovery primitives while the
// dedicated `windows_install` module owns replacement, health checking, locking, and rollback.
#[cfg_attr(windows, allow(dead_code))]
mod install;
pub mod manifest;
mod model;
mod release_id;
mod source;
#[cfg(windows)]
mod windows_install;

/// Release identity for the current build. Cargo package metadata stays at
/// `1.0.7` because Cargo accepts only SemVer package versions.
pub const CURRENT_RELEASE_ID: &str = "1.0.7.1";

pub use diagnostics::{PhaseTimer, UpdateDiagnostics, UpdatePhase, UpdatePhaseRecord};
pub use github::{GithubReleaseClient, ReleaseClient};
#[cfg(not(windows))]
pub use install::AtomicInstaller;
pub use install::{
    HEALTH_FILE_ENV, InstallKind, InstallLocation, Restart, ScheduledUpdate,
    acknowledge_update_health,
};
pub use manifest::{
    Architecture, ArtifactKind, BuildSource, DEFAULT_KEY_ID, KeyStatus, MANIFEST_NAME,
    MANIFEST_SCHEMA, ManifestArtifact, ManifestError, ManifestSignature, OperatingSystem, Platform,
    ReleaseEvidence, ReleaseManifest, RolloutPolicy, SIGNATURE_NAME, SIGNATURE_SCHEMA, TrustStore,
    TrustedKey, VerifiedManifest,
};
pub use model::{
    Artifact, DownloadReport, Release, UpdateCheck, UpdatePolicy, copy_with_progress,
    verify_artifact, verify_sha256,
};
pub use release_id::ReleaseId;
pub use source::{
    MainArtifactPhase, MainArtifactProgress, MainBranchRevision, MainBranchUpdater,
    MainBuildProgress, rolling_desktop_asset_name,
};
#[cfg(windows)]
pub use windows_install::AtomicInstaller;
