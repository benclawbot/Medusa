use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use crate::verification_dag::VerificationDag;

const CHECKPOINT_SCHEMA_VERSION: u16 = 1;

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
    path: PathBuf,
}

impl VerificationCheckpointStore {
    pub fn new(store_root: &Path) -> Self {
        Self {
            path: store_root.join("verification-checkpoint.json"),
        }
    }

    pub fn load<T>(
        &self,
        expected_repository_state_fingerprint: &str,
    ) -> Result<Option<VerificationCheckpoint<T>>, String>
    where
        T: Clone + DeserializeOwned + Serialize,
    {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let checkpoint = match serde_json::from_slice::<VerificationCheckpoint<T>>(&bytes) {
            Ok(checkpoint) => checkpoint,
            Err(_) => {
                self.remove()?;
                return Ok(None);
            }
        };
        if checkpoint
            .validate(expected_repository_state_fingerprint)
            .is_err()
        {
            self.remove()?;
            return Ok(None);
        }
        Ok(Some(checkpoint))
    }

    pub fn save<T>(&self, checkpoint: &VerificationCheckpoint<T>) -> Result<(), String>
    where
        T: Clone + Serialize,
    {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "verification checkpoint path has no parent".to_owned())?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let bytes = serde_json::to_vec_pretty(checkpoint).map_err(|error| error.to_string())?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos();
        let temporary = parent.join(format!(
            ".verification-checkpoint.tmp-{}-{nonce}",
            std::process::id()
        ));
        fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
        match fs::rename(&temporary, &self.path) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                Err(error.to_string())
            }
        }
    }

    pub fn remove(&self) -> Result<(), String> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
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
        let checkpoint = VerificationCheckpoint::new("state-a", dag(), vec!["complete"])
            .expect("checkpoint");
        store.save(&checkpoint).expect("save");

        let restored = store
            .load::<Vec<String>>("state-a")
            .expect("load")
            .expect("checkpoint");
        assert_eq!(restored.payload, vec!["complete"]);
        assert!(store.load::<Vec<String>>("state-b").expect("load").is_none());
    }

    #[test]
    fn corrupt_checkpoint_is_removed_and_fails_closed() {
        let directory = tempfile::tempdir().expect("directory");
        let store = VerificationCheckpointStore::new(directory.path());
        fs::write(&store.path, b"{broken").expect("corrupt checkpoint");
        assert!(store.load::<Vec<String>>("state").expect("load").is_none());
        assert!(!store.path.exists());
    }
}
