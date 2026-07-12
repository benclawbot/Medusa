use std::{fs, path::{Path, PathBuf}, process::Command};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::support::{invalid, now};

/// Release artifact entry with checksum and size.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArtifactEntry {
    pub path: PathBuf,
    pub sha256: String,
    pub size: u64,
}

/// Reproducible release manifest used by package smoke tests.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReleaseManifest {
    pub version: String,
    pub target: String,
    pub artifacts: Vec<ArtifactEntry>,
    pub generated_at: String,
    pub sbom: PathBuf,
    pub rollback_instructions: PathBuf,
}

pub fn build_release_manifest(
    version: &str,
    target: &str,
    artifact_paths: &[PathBuf],
    sbom: PathBuf,
    rollback_instructions: PathBuf,
) -> MedusaResult<ReleaseManifest> {
    if version.trim().is_empty() || target.trim().is_empty() || artifact_paths.is_empty() {
        return Err(invalid(
            "release manifest requires version, target, and artifacts",
        ));
    }
    let mut artifacts = artifact_paths
        .iter()
        .map(|path| {
            let bytes = fs::read(path)?;
            Ok(ArtifactEntry {
                path: path.clone(),
                sha256: format!("{:x}", Sha256::digest(&bytes)),
                size: bytes.len() as u64,
            })
        })
        .collect::<MedusaResult<Vec<_>>>()?;
    artifacts.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(ReleaseManifest {
        version: version.into(),
        target: target.into(),
        artifacts,
        generated_at: now()?,
        sbom,
        rollback_instructions,
    })
}

/// Executes installation/package smoke checks for a built binary.
pub fn package_smoke(binary: &Path) -> MedusaResult<String> {
    let metadata = fs::metadata(binary)?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(invalid("package binary is missing or empty"));
    }
    let output = Command::new(binary).arg("--version").output()?;
    if !output.status.success() {
        return Err(MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
