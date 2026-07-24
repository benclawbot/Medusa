use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchIntent {
    pub worker_id: String,
    pub lease_epoch: u64,
    pub path: String,
    pub base_fingerprint: String,
    pub replacement_fingerprint: String,
    pub priority: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resolution {
    Apply(PatchIntent),
    Identical { path: String, workers: Vec<String> },
    Conflict(ConflictEvidence),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictEvidence {
    pub path: String,
    pub workers: Vec<String>,
    pub base_fingerprints: Vec<String>,
    pub replacement_fingerprints: Vec<String>,
    pub fingerprint: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ResolutionError {
    #[error("invalid repository path: {0}")]
    InvalidPath(String),
    #[error("invalid fingerprint for worker {0}")]
    InvalidFingerprint(String),
    #[error("duplicate worker intent for path {path}: {worker}")]
    DuplicateIntent { path: String, worker: String },
    #[error("stale lease epoch for worker {worker}: {actual} < {expected}")]
    StaleLease { worker: String, actual: u64, expected: u64 },
}

#[derive(Debug, Default)]
pub struct ConflictResolver;

impl ConflictResolver {
    pub fn resolve(
        &self,
        intents: impl IntoIterator<Item = PatchIntent>,
        required_epochs: &BTreeMap<String, u64>,
    ) -> Result<Vec<Resolution>, ResolutionError> {
        let mut by_path: BTreeMap<String, Vec<PatchIntent>> = BTreeMap::new();
        let mut seen = BTreeSet::new();

        for intent in intents {
            validate_path(&intent.path)?;
            validate_fingerprint(&intent.base_fingerprint, &intent.worker_id)?;
            validate_fingerprint(&intent.replacement_fingerprint, &intent.worker_id)?;
            if let Some(expected) = required_epochs.get(&intent.worker_id) {
                if intent.lease_epoch < *expected {
                    return Err(ResolutionError::StaleLease {
                        worker: intent.worker_id,
                        actual: intent.lease_epoch,
                        expected: *expected,
                    });
                }
            }
            let key = (intent.path.clone(), intent.worker_id.clone());
            if !seen.insert(key.clone()) {
                return Err(ResolutionError::DuplicateIntent {
                    path: key.0,
                    worker: key.1,
                });
            }
            by_path.entry(intent.path.clone()).or_default().push(intent);
        }

        let mut resolutions = Vec::with_capacity(by_path.len());
        for (path, mut candidates) in by_path {
            candidates.sort_by(|a, b| {
                a.priority
                    .cmp(&b.priority)
                    .then_with(|| b.lease_epoch.cmp(&a.lease_epoch))
                    .then_with(|| a.worker_id.cmp(&b.worker_id))
            });

            if candidates.len() == 1 {
                resolutions.push(Resolution::Apply(candidates.remove(0)));
                continue;
            }

            let replacements: BTreeSet<_> = candidates
                .iter()
                .map(|candidate| candidate.replacement_fingerprint.clone())
                .collect();
            if replacements.len() == 1 {
                resolutions.push(Resolution::Identical {
                    path,
                    workers: candidates.into_iter().map(|candidate| candidate.worker_id).collect(),
                });
                continue;
            }

            resolutions.push(Resolution::Conflict(conflict_evidence(path, &candidates)));
        }
        Ok(resolutions)
    }
}

fn validate_path(path: &str) -> Result<(), ResolutionError> {
    let unsafe_path = path.is_empty()
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.split(['/', '\\']).any(|part| part == ".." || part.is_empty());
    if unsafe_path {
        return Err(ResolutionError::InvalidPath(path.to_owned()));
    }
    Ok(())
}

fn validate_fingerprint(value: &str, worker: &str) -> Result<(), ResolutionError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ResolutionError::InvalidFingerprint(worker.to_owned()));
    }
    Ok(())
}

fn conflict_evidence(path: String, candidates: &[PatchIntent]) -> ConflictEvidence {
    let workers: Vec<_> = candidates.iter().map(|item| item.worker_id.clone()).collect();
    let base_fingerprints: Vec<_> = candidates
        .iter()
        .map(|item| item.base_fingerprint.clone())
        .collect();
    let replacement_fingerprints: Vec<_> = candidates
        .iter()
        .map(|item| item.replacement_fingerprint.clone())
        .collect();
    let mut hasher = Sha256::new();
    hasher.update(path.as_bytes());
    for item in candidates {
        hasher.update(item.worker_id.as_bytes());
        hasher.update(item.lease_epoch.to_be_bytes());
        hasher.update(item.base_fingerprint.as_bytes());
        hasher.update(item.replacement_fingerprint.as_bytes());
        hasher.update(item.priority.to_be_bytes());
    }
    ConflictEvidence {
        path,
        workers,
        base_fingerprints,
        replacement_fingerprints,
        fingerprint: hex::encode(hasher.finalize()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }

    fn intent(worker: &str, path: &str, replacement: char) -> PatchIntent {
        PatchIntent {
            worker_id: worker.into(),
            lease_epoch: 4,
            path: path.into(),
            base_fingerprint: fp('a'),
            replacement_fingerprint: fp(replacement),
            priority: 10,
        }
    }

    #[test]
    fn returns_paths_in_deterministic_order() {
        let result = ConflictResolver
            .resolve([intent("b", "z.rs", 'b'), intent("a", "a.rs", 'c')], &BTreeMap::new())
            .unwrap();
        assert!(matches!(&result[0], Resolution::Apply(value) if value.path == "a.rs"));
        assert!(matches!(&result[1], Resolution::Apply(value) if value.path == "z.rs"));
    }

    #[test]
    fn coalesces_identical_replacements() {
        let result = ConflictResolver
            .resolve([intent("b", "src/lib.rs", 'b'), intent("a", "src/lib.rs", 'b')], &BTreeMap::new())
            .unwrap();
        assert_eq!(result, vec![Resolution::Identical {
            path: "src/lib.rs".into(),
            workers: vec!["a".into(), "b".into()],
        }]);
    }

    #[test]
    fn emits_stable_conflict_evidence() {
        let first = ConflictResolver
            .resolve([intent("b", "src/lib.rs", 'b'), intent("a", "src/lib.rs", 'c')], &BTreeMap::new())
            .unwrap();
        let second = ConflictResolver
            .resolve([intent("a", "src/lib.rs", 'c'), intent("b", "src/lib.rs", 'b')], &BTreeMap::new())
            .unwrap();
        assert_eq!(first, second);
        assert!(matches!(&first[0], Resolution::Conflict(value) if value.fingerprint.len() == 64));
    }

    #[test]
    fn rejects_stale_leases() {
        let epochs = BTreeMap::from([("a".into(), 5)]);
        assert!(matches!(
            ConflictResolver.resolve([intent("a", "src/lib.rs", 'b')], &epochs),
            Err(ResolutionError::StaleLease { .. })
        ));
    }

    #[test]
    fn rejects_duplicate_worker_intents() {
        let change = intent("a", "src/lib.rs", 'b');
        assert!(matches!(
            ConflictResolver.resolve([change.clone(), change], &BTreeMap::new()),
            Err(ResolutionError::DuplicateIntent { .. })
        ));
    }

    #[test]
    fn rejects_unsafe_paths() {
        assert_eq!(
            ConflictResolver.resolve([intent("a", "../secret", 'b')], &BTreeMap::new()),
            Err(ResolutionError::InvalidPath("../secret".into()))
        );
    }
}
