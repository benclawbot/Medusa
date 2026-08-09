use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use crate::verification_dag::VerificationDag;

const CHECKPOINT_SCHEMA_VERSION: u16 = 1;
const CHECKPOINT_PREFIX: &str = "verification-checkpoint-";
const CHECKPOINT_SUFFIX: &str = ".json";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VerificationCheckpoint<T> {
    schema_version: u16,
    repository_state_fingerprint: String,
    pub dag: VerificationDag,
    pub payload: T,
    fingerprint: String,
}

impl<T> VerificationCheckpoint<T>
where
    T: Clone + Serialize,
{
    pub fn new(
        repository_state_fingerprint: &str,
        dag: VerificationDag,
        payload: T,
    ) -> Result<Self, String> {
        if repository_state_fingerprint.trim().is_empty() {
            return Err("verification checkpoint requires repository state identity".to_owned());
        }
        let mut checkpoint = Self {
            schema_version: CHECKPOINT_SCHEMA_VERSION,
            repository_state_fingerprint: repository_state_fingerprint.to_owned(),
            dag,
            payload,
            fingerprint: String::new(),
        };
        checkpoint.fingerprint = checkpoint_fingerprint(&checkpoint)?;
        Ok(checkpoint)
    }

    fn validate(&self, expected_repository_state_fingerprint: &str) -> Result<(), String> {
        if self.schema_version != CHECKPOINT_SCHEMA_VERSION
            || self.repository_state_fingerprint != expected_repository_state_fingerprint
            || self.fingerprint != checkpoint_fingerprint(self)?
        {
            return Err("verification checkpoint is stale or corrupted".to_owned());
        }
        Ok(())
    }
}

pub struct VerificationCheckpointStore {
    root: PathBuf,
}

impl VerificationCheckpointStore {
    pub fn new(store_root: &Path) -> Self {
        Self {
            root: store_root.to_path_buf(),
        }
    }

    pub fn load<T>(
        &self,
        expected_repository_state_fingerprint: &str,
    ) -> Result<Option<VerificationCheckpoint<T>>, String>
    where
        T: Clone + DeserializeOwned + Serialize,
    {
        let mut generations = self.generations()?;
        generations.sort();
        generations.reverse();
        for path in generations {
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.to_string()),
            };
            let checkpoint = match serde_json::from_slice::<VerificationCheckpoint<T>>(&bytes) {
                Ok(checkpoint) => checkpoint,
                Err(_) => {
                    remove_file_if_present(&path)?;
                    continue;
                }
            };
            if checkpoint
                .validate(expected_repository_state_fingerprint)
                .is_err()
            {
                remove_file_if_present(&path)?;
                continue;
            }
            return Ok(Some(checkpoint));
        }
        Ok(None)
    }

    pub fn save<T>(&self, checkpoint: &VerificationCheckpoint<T>) -> Result<(), String>
    where
        T: Clone + Serialize,
    {
        fs::create_dir_all(&self.root).map_err(|error| error.to_string())?;
        let bytes = serde_json::to_vec_pretty(checkpoint).map_err(|error| error.to_string())?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos();
        let generation = format!("{nonce:039}-{:010}", std::process::id());
        let temporary = self
            .root
            .join(format!(".{CHECKPOINT_PREFIX}{generation}.tmp"));
        let published = self.root.join(format!(
            "{CHECKPOINT_PREFIX}{generation}{CHECKPOINT_SUFFIX}"
        ));
        fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
        match fs::rename(&temporary, &published) {
            Ok(()) => {
                self.prune_except(&published)?;
                Ok(())
            }
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                Err(error.to_string())
            }
        }
    }

    pub fn remove(&self) -> Result<(), String> {
        for path in self.generations()? {
            remove_file_if_present(&path)?;
        }
        if self.root.is_dir() {
            for entry in fs::read_dir(&self.root).map_err(|error| error.to_string())? {
                let entry = entry.map_err(|error| error.to_string())?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(&format!(".{CHECKPOINT_PREFIX}")) && name.ends_with(".tmp") {
                    remove_file_if_present(&entry.path())?;
                }
            }
        }
        Ok(())
    }

    fn generations(&self) -> Result<Vec<PathBuf>, String> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut result = Vec::new();
        for entry in fs::read_dir(&self.root).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(CHECKPOINT_PREFIX) && name.ends_with(CHECKPOINT_SUFFIX) {
                result.push(entry.path());
            }
        }
        Ok(result)
    }

    fn prune_except(&self, keep: &Path) -> Result<(), String> {
        for path in self.generations()? {
            if path != keep {
                remove_file_if_present(&path)?;
            }
        }
        Ok(())
    }
}

fn remove_file_if_present(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn checkpoint_fingerprint<T>(checkpoint: &VerificationCheckpoint<T>) -> Result<String, String>
where
    T: Serialize,
{
    let mut hasher = Sha256::new();
    hasher.update(checkpoint.schema_version.to_le_bytes());
    hasher.update(checkpoint.repository_state_fingerprint.as_bytes());
    hasher.update(serde_json::to_vec(&checkpoint.dag).map_err(|error| error.to_string())?);
    hasher.update(serde_json::to_vec(&checkpoint.payload).map_err(|error| error.to_string())?);
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::verification_dag::{
        VerificationAuthority, VerificationInputKey, VerificationNode, VerificationNodeState,
    };

    fn dag() -> VerificationDag {
        let mut dag = VerificationDag::default();
        dag.insert(VerificationNode {
            id: "unit".to_owned(),
            command: "cargo test".to_owned(),
            dependencies: BTreeSet::new(),
            authority: VerificationAuthority::IndependentAcceptance,
            expected_duration_ms: 10,
            resource_class: "cpu".to_owned(),
            input: VerificationInputKey {
                repository_revision: "rev".to_owned(),
                tree_fingerprint: "tree".to_owned(),
                environment_fingerprint: "env".to_owned(),
                toolchain_fingerprint: "toolchain".to_owned(),
                adapter_version: "adapter".to_owned(),
                changed_paths: ["src/lib.rs".to_owned()].into_iter().collect(),
            },
            state: VerificationNodeState::Pending,
        })
        .expect("node");
        dag
    }

    #[test]
    fn roundtrip_is_bound_to_exact_repository_state() {
        let directory = tempfile::tempdir().expect("directory");
        let store = VerificationCheckpointStore::new(directory.path());
        let checkpoint =
            VerificationCheckpoint::new("state-a", dag(), vec!["complete"]).expect("checkpoint");
        store.save(&checkpoint).expect("save");

        let restored = store
            .load::<Vec<String>>("state-a")
            .expect("load")
            .expect("checkpoint");
        assert_eq!(restored.payload, vec!["complete"]);
        assert!(
            store
                .load::<Vec<String>>("state-b")
                .expect("load")
                .is_none()
        );
    }

    #[test]
    fn newer_generation_replaces_older_without_target_overwrite() {
        let directory = tempfile::tempdir().expect("directory");
        let store = VerificationCheckpointStore::new(directory.path());
        store
            .save(&VerificationCheckpoint::new("state", dag(), vec!["first"]).expect("first"))
            .expect("save first");
        store
            .save(&VerificationCheckpoint::new("state", dag(), vec!["second"]).expect("second"))
            .expect("save second");
        let restored = store
            .load::<Vec<String>>("state")
            .expect("load")
            .expect("checkpoint");
        assert_eq!(restored.payload, vec!["second"]);
        assert_eq!(store.generations().expect("generations").len(), 1);
    }

    #[test]
    fn corrupt_checkpoint_is_removed_and_fails_closed() {
        let directory = tempfile::tempdir().expect("directory");
        let store = VerificationCheckpointStore::new(directory.path());
        fs::create_dir_all(directory.path()).expect("directory");
        let corrupt = directory.path().join(
            "verification-checkpoint-000000000000000000000000000000000000001-0000000001.json",
        );
        fs::write(&corrupt, b"{broken").expect("corrupt checkpoint");
        assert!(store.load::<Vec<String>>("state").expect("load").is_none());
        assert!(!corrupt.exists());
    }
}
