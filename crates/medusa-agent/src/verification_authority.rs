use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_evidence::{ArtifactId, ChangeKind, ChangedComponent, VerificationReceipt};
use sha2::{Digest, Sha256};

use crate::verification::VerificationResult;

#[path = "verification_checkpoint.rs"]
pub(crate) mod verification_checkpoint;

use verification_checkpoint::VerificationCheckpointStore;

#[allow(dead_code)]
#[path = "verification_authority_legacy.rs"]
mod legacy;

pub use legacy::{AuthoritativeVerificationResult, prepare_components_for_verification};

const MAX_STABLE_VERIFICATION_ATTEMPTS: usize = 3;
const STATE_FINGERPRINT_FILE: &str = "verification-state-fingerprint";

pub(crate) fn prepare_paths_for_verification(repo: &Path, paths: &[String]) -> MedusaResult<()> {
    legacy::prepare_paths_for_verification(repo, paths)
}

pub fn authoritative_verification_for_components(
    repo: &Path,
    repository_fingerprint: &str,
    commit: &str,
    components: &[ChangedComponent],
) -> MedusaResult<AuthoritativeVerificationResult> {
    authoritative_verification_for_components_at(
        repo,
        &repo.join(".medusa/evidence"),
        repository_fingerprint,
        commit,
        components,
    )
}

pub fn authoritative_verification_for_components_at(
    repo: &Path,
    evidence_root: &Path,
    repository_fingerprint: &str,
    commit: &str,
    components: &[ChangedComponent],
) -> MedusaResult<AuthoritativeVerificationResult> {
    fs::create_dir_all(evidence_root)?;
    let store_root = evidence_root.join(short_hash(&(repository_fingerprint.to_owned() + commit)));

    for _ in 0..MAX_STABLE_VERIFICATION_ATTEMPTS {
        let before = complete_repository_state_fingerprint(repo, evidence_root, components)?;
        if !persisted_receipt_safe(&store_root) || !state_marker_matches(&store_root, &before) {
            invalidate_persisted_reuse(&store_root)?;
        }

        let result = legacy::authoritative_verification_for_components_at(
            repo,
            evidence_root,
            repository_fingerprint,
            commit,
            components,
        )?;
        let after = complete_repository_state_fingerprint(repo, evidence_root, components)?;
        if before == after {
            fs::create_dir_all(&store_root)?;
            fs::write(store_root.join(STATE_FINGERPRINT_FILE), after)?;
            return Ok(result);
        }

        invalidate_persisted_reuse(&store_root)?;
    }

    Err(invalid(
        "repository state changed repeatedly during authoritative verification",
    ))
}

fn state_marker_matches(store_root: &Path, expected: &str) -> bool {
    fs::read_to_string(store_root.join(STATE_FINGERPRINT_FILE))
        .is_ok_and(|actual| actual == expected)
}

fn persisted_receipt_safe(store_root: &Path) -> bool {
    let path = store_root.join("verification-receipt.json");
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
        Err(_) => return false,
    };
    let receipt = match serde_json::from_slice::<VerificationReceipt>(&bytes) {
        Ok(receipt) => receipt,
        Err(_) => return false,
    };
    receipt
        .evidence
        .artifacts
        .iter()
        .all(|artifact| valid_cache_artifact_id(&artifact.id))
}

fn valid_cache_artifact_id(id: &ArtifactId) -> bool {
    id.0.strip_prefix("artifact-").is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn invalidate_persisted_reuse(store_root: &Path) -> MedusaResult<()> {
    VerificationCheckpointStore::new(store_root)
        .remove()
        .map_err(|error| {
            invalid(format!(
                "failed to invalidate verification checkpoint: {error}"
            ))
        })?;
    for name in [
        "verification-receipt.json",
        "verification-dag.json",
        STATE_FINGERPRINT_FILE,
    ] {
        match fs::remove_file(store_root.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn complete_repository_state_fingerprint(
    repo: &Path,
    evidence_root: &Path,
    components: &[ChangedComponent],
) -> MedusaResult<String> {
    let mut paths = repository_state_paths(repo, components)?;
    paths.sort();
    paths.dedup();
    let evidence_root = evidence_root
        .canonicalize()
        .unwrap_or_else(|_| evidence_root.to_path_buf());
    let mut hasher = Sha256::new();
    for relative in paths {
        let path = repo.join(&relative);
        let absolute = path.canonicalize().unwrap_or_else(|_| path.clone());
        if absolute.starts_with(&evidence_root) || relative.starts_with(".git") {
            continue;
        }
        let relative = relative.to_string_lossy();
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => match fs::read_link(&path) {
                Ok(target) => hash_repository_state_entry(
                    &mut hasher,
                    relative.as_bytes(),
                    b"symlink",
                    target.to_string_lossy().as_bytes(),
                ),
                Err(_) => hash_repository_state_entry(
                    &mut hasher,
                    relative.as_bytes(),
                    b"symlink",
                    b"unreadable",
                ),
            },
            Ok(metadata) if metadata.is_file() => match fs::read(&path) {
                Ok(bytes) => {
                    hash_repository_state_entry(&mut hasher, relative.as_bytes(), b"file", &bytes)
                }
                Err(_) => hash_repository_state_entry(
                    &mut hasher,
                    relative.as_bytes(),
                    b"file",
                    b"unreadable",
                ),
            },
            Ok(_) => hash_repository_state_entry(&mut hasher, relative.as_bytes(), b"other", b""),
            Err(_) => {
                hash_repository_state_entry(&mut hasher, relative.as_bytes(), b"missing", b"")
            }
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn repository_state_paths(
    repo: &Path,
    components: &[ChangedComponent],
) -> MedusaResult<Vec<PathBuf>> {
    let mut result = if git_repository(repo) {
        let output = Command::new("git")
            .args([
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ])
            .current_dir(repo)
            .output()?;
        if output.status.success() {
            output
                .stdout
                .split(|byte| *byte == 0)
                .filter(|entry| !entry.is_empty())
                .map(|entry| PathBuf::from(String::from_utf8_lossy(entry).into_owned()))
                .collect()
        } else {
            collect_repository_paths(repo)?
        }
    } else {
        collect_repository_paths(repo)?
    };

    for component in components {
        result.extend(component.all_paths().into_iter().map(PathBuf::from));
    }
    Ok(result)
}

fn collect_repository_paths(repo: &Path) -> MedusaResult<Vec<PathBuf>> {
    fn collect(root: &Path, directory: &Path, result: &mut Vec<PathBuf>) -> MedusaResult<()> {
        let mut entries = fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            if relative.starts_with(".git") {
                continue;
            }
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                collect(root, &path, result)?;
            } else {
                result.push(relative);
            }
        }
        Ok(())
    }

    let mut result = Vec::new();
    collect(repo, repo, &mut result)?;
    Ok(result)
}

fn hash_repository_state_entry(hasher: &mut Sha256, path: &[u8], kind: &[u8], payload: &[u8]) {
    for part in [path, kind, payload] {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
}

fn git_repository(repo: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(repo)
        .output()
        .is_ok_and(|output| output.status.success() && output.stdout.starts_with(b"true"))
}

fn git_stdout(repo: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn short_hash(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn evidence_error(error: medusa_evidence::EvidenceError) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        error.to_string(),
    )
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

pub(crate) fn authoritative_verification_for_paths(
    repo: &Path,
    paths: &[String],
) -> MedusaResult<VerificationResult> {
    let components = paths
        .iter()
        .map(|path| ChangedComponent::new(ChangeKind::Modified, path.clone()))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(evidence_error)?;
    let commit = git_stdout(repo, &["rev-parse", "HEAD"]).unwrap_or_else(|| "worktree".to_owned());
    let repository_fingerprint = short_hash(&format!("{}:{commit}", repo.display()));
    let result = authoritative_verification_for_components(
        repo,
        &repository_fingerprint,
        &commit,
        &components,
    )?;
    Ok(VerificationResult {
        passed: result.receipt.passed,
        evidence: result.summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_ignored_component_changes_repository_state_fingerprint() {
        let directory = tempfile::tempdir().expect("repository");
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(directory.path())
                .status()
                .expect("git");
            assert!(status.success());
        };
        git(&["init", "-q"]);
        fs::write(directory.path().join(".gitignore"), "artifact.json\n").expect("gitignore");
        fs::write(directory.path().join("artifact.json"), "{\"ok\":true}\n").expect("artifact");
        let component =
            ChangedComponent::new(ChangeKind::Modified, "artifact.json").expect("component");
        let evidence_root = directory.path().join(".medusa/evidence");
        fs::create_dir_all(&evidence_root).expect("evidence root");

        let before = complete_repository_state_fingerprint(
            directory.path(),
            &evidence_root,
            &[component.clone()],
        )
        .expect("before fingerprint");
        fs::write(directory.path().join("artifact.json"), "{broken}\n").expect("mutate");
        let after =
            complete_repository_state_fingerprint(directory.path(), &evidence_root, &[component])
                .expect("after fingerprint");

        assert_ne!(before, after);
    }

    #[test]
    fn cache_artifact_ids_reject_path_traversal() {
        assert!(!valid_cache_artifact_id(&ArtifactId(
            "artifact-../../../../../tmp/victim".to_owned()
        )));
        assert!(valid_cache_artifact_id(&ArtifactId(format!(
            "artifact-{}",
            "a".repeat(64)
        ))));
    }

    #[test]
    fn outer_state_guard_reuses_only_stable_semantic_inputs() {
        let directory = tempfile::tempdir().expect("repository");
        fs::write(directory.path().join("artifact.json"), "{\"ok\":true}\n").expect("artifact");
        let component =
            ChangedComponent::new(ChangeKind::Modified, "artifact.json").expect("component");
        let evidence_root = directory.path().join("durable-evidence");

        let first = authoritative_verification_for_components_at(
            directory.path(),
            &evidence_root,
            "repo",
            "commit",
            &[component.clone()],
        )
        .expect("first verification");
        assert!(first.receipt.passed);

        let reused = authoritative_verification_for_components_at(
            directory.path(),
            &evidence_root,
            "repo",
            "commit",
            &[component.clone()],
        )
        .expect("reuse");
        assert!(
            reused
                .summary
                .iter()
                .any(|line| line == "verification_reuse=exact-persisted-receipt")
        );

        fs::write(directory.path().join("artifact.json"), "{broken}\n").expect("mutate");
        let changed = authoritative_verification_for_components_at(
            directory.path(),
            &evidence_root,
            "repo",
            "commit",
            &[component],
        )
        .expect("changed verification");
        assert!(!changed.receipt.passed);
        assert!(
            !changed
                .summary
                .iter()
                .any(|line| line == "verification_reuse=exact-persisted-receipt")
        );
    }
}
