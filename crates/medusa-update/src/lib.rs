//! Verified prebuilt-release self-update primitives.
//!
//! The CLI owns user interaction; this crate owns signed release discovery,
//! exact artifact verification, confined extraction, diagnostics, and a
//! health-checked atomic installation with rollback.

mod diagnostics;
mod github;
mod install;
pub mod manifest;
mod model;
mod source;

pub use diagnostics::{PhaseTimer, UpdateDiagnostics, UpdatePhase, UpdatePhaseRecord};
pub use github::{GithubReleaseClient, ReleaseClient};
pub use install::{
    acknowledge_update_health, AtomicInstaller, InstallKind, InstallLocation, Restart,
    ScheduledUpdate, HEALTH_FILE_ENV,
};
pub use manifest::{
    Architecture, ArtifactKind, BuildSource, KeyStatus, ManifestArtifact, ManifestError,
    ManifestSignature, OperatingSystem, Platform, ReleaseManifest, RolloutPolicy, TrustStore,
    TrustedKey, VerifiedManifest, DEFAULT_KEY_ID, MANIFEST_NAME, MANIFEST_SCHEMA, SIGNATURE_NAME,
    SIGNATURE_SCHEMA,
};
pub use model::{
    verify_artifact, verify_sha256, Artifact, DownloadReport, Release, UpdateCheck, UpdatePolicy,
};
pub use source::{MainBranchRevision, MainBranchUpdater};
