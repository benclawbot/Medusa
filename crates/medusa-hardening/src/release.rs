use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn manifest_requires_complete_inputs_and_existing_artifacts() {
        let directory = tempfile::tempdir().expect("tempdir");
        let sbom = directory.path().join("sbom.json");
        let rollback = directory.path().join("ROLLBACK.md");
        assert!(
            build_release_manifest("", "target", &[], sbom.clone(), rollback.clone()).is_err()
        );
        assert!(
            build_release_manifest(
                "1.0.0",
                "target",
                &[directory.path().join("missing")],
                sbom,
                rollback,
            )
            .is_err()
        );
    }

    #[test]
    fn manifest_sorts_artifacts_and_records_sizes() {
        let directory = tempfile::tempdir().expect("tempdir");
        let first = directory.path().join("a.bin");
        let second = directory.path().join("z.bin");
        fs::write(&first, b"a").expect("first");
        fs::write(&second, b"zz").expect("second");
        let manifest = build_release_manifest(
            "1.2.3",
            "test-target",
            &[second.clone(), first.clone()],
            directory.path().join("sbom.json"),
            directory.path().join("ROLLBACK.md"),
        )
        .expect("manifest");
        assert_eq!(manifest.artifacts[0].path, first);
        assert_eq!(manifest.artifacts[0].size, 1);
        assert_eq!(manifest.artifacts[1].path, second);
        assert_eq!(manifest.artifacts[1].size, 2);
    }

    #[cfg(unix)]
    #[test]
    fn package_smoke_accepts_successful_binary_and_rejects_failures() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("tempdir");
        let success = directory.path().join("success.sh");
        fs::write(&success, "#!/bin/sh\necho medusa-test 1.0.0\n").expect("success");
        fs::set_permissions(&success, fs::Permissions::from_mode(0o700)).expect("permissions");
        assert_eq!(
            package_smoke(&success).expect("smoke"),
            "medusa-test 1.0.0"
        );

        let failure = directory.path().join("failure.sh");
        fs::write(&failure, "#!/bin/sh\necho broken >&2\nexit 7\n").expect("failure");
        fs::set_permissions(&failure, fs::Permissions::from_mode(0o700)).expect("permissions");
        assert!(package_smoke(&failure).is_err());

        let empty = directory.path().join("empty");
        fs::write(&empty, b"").expect("empty");
        assert!(package_smoke(&empty).is_err());
    }
}
