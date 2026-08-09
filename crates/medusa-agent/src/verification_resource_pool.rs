use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ulid::Ulid;

const RESOURCE_POOL_SCHEMA_VERSION: u16 = 1;
const METADATA_SUFFIX: &str = ".json";
const OBJECT_SUFFIX: &str = ".bin";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarmResourceKind {
    BuildMetadata,
    DependencyCache,
    Worktree,
    BrowserFixture,
    Sidecar,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WarmResourceKey {
    pub repository_fingerprint: String,
    pub branch_fingerprint: String,
    pub trust_domain: String,
    pub resource_kind: WarmResourceKind,
    pub input_fingerprint: String,
    pub toolchain_fingerprint: String,
}

impl WarmResourceKey {
    fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            (
                "repository fingerprint",
                self.repository_fingerprint.as_str(),
            ),
            ("branch fingerprint", self.branch_fingerprint.as_str()),
            ("trust domain", self.trust_domain.as_str()),
            ("input fingerprint", self.input_fingerprint.as_str()),
            ("toolchain fingerprint", self.toolchain_fingerprint.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(format!("warm verification resource requires {name}"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WarmResourcePoolLimits {
    pub max_entries: usize,
    pub max_bytes: u64,
    pub max_age_seconds: u64,
}

impl Default for WarmResourcePoolLimits {
    fn default() -> Self {
        Self {
            max_entries: 32,
            max_bytes: 2 * 1024 * 1024 * 1024,
            max_age_seconds: 24 * 60 * 60,
        }
    }
}

impl WarmResourcePoolLimits {
    fn validate(self) -> Result<Self, String> {
        if self.max_entries == 0 || self.max_bytes == 0 || self.max_age_seconds == 0 {
            return Err("warm verification resource limits must be non-zero".to_owned());
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WarmResourceReceipt {
    pub schema_version: u16,
    pub key: WarmResourceKey,
    pub object_sha256: String,
    pub byte_len: u64,
    pub created_unix_seconds: u64,
}

#[derive(Clone, Debug)]
pub struct WarmResourcePool {
    root: PathBuf,
    limits: WarmResourcePoolLimits,
    operation_lock: Arc<Mutex<()>>,
}

impl WarmResourcePool {
    pub fn open(root: impl Into<PathBuf>, limits: WarmResourcePoolLimits) -> Result<Self, String> {
        let limits = limits.validate()?;
        let root = root.into();
        fs::create_dir_all(&root).map_err(|error| {
            format!(
                "failed to create warm verification resource pool {}: {error}",
                root.display()
            )
        })?;
        let pool = Self {
            root,
            limits,
            operation_lock: Arc::new(Mutex::new(())),
        };
        pool.prune()?;
        Ok(pool)
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn put(&self, key: WarmResourceKey, bytes: &[u8]) -> Result<WarmResourceReceipt, String> {
        key.validate()?;
        if bytes.len() as u64 > self.limits.max_bytes {
            return Err(format!(
                "warm verification resource is {} bytes, above pool limit {}",
                bytes.len(),
                self.limits.max_bytes
            ));
        }
        let _guard = self.lock_operations()?;
        self.prune_unlocked()?;
        let entry_id = key_fingerprint(&key)?;
        let object_path = self.object_path(&entry_id);
        let metadata_path = self.metadata_path(&entry_id);
        if object_path.is_file() || metadata_path.is_file() {
            if let Some(existing) = self.get_unlocked(&key)?
                && existing.1 == bytes
            {
                return Ok(existing.0);
            }
            remove_pair(&object_path, &metadata_path)?;
        }

        let receipt = WarmResourceReceipt {
            schema_version: RESOURCE_POOL_SCHEMA_VERSION,
            key,
            object_sha256: sha256(bytes),
            byte_len: bytes.len() as u64,
            created_unix_seconds: now_unix_seconds()?,
        };
        write_new_atomic(&object_path, bytes)?;
        let metadata = serde_json::to_vec_pretty(&receipt)
            .map_err(|error| format!("failed to serialize warm resource receipt: {error}"))?;
        if let Err(error) = write_new_atomic(&metadata_path, &metadata) {
            let _ = fs::remove_file(&object_path);
            return Err(error);
        }
        self.prune_unlocked()?;

        match self.get_unlocked(&receipt.key)? {
            Some((persisted, _)) => Ok(persisted),
            None => {
                Err("warm verification resource was immediately evicted by pool limits".to_owned())
            }
        }
    }

    pub fn get(
        &self,
        key: &WarmResourceKey,
    ) -> Result<Option<(WarmResourceReceipt, Vec<u8>)>, String> {
        key.validate()?;
        let _guard = self.lock_operations()?;
        self.get_unlocked(key)
    }

    pub fn prune(&self) -> Result<usize, String> {
        let _guard = self.lock_operations()?;
        self.prune_unlocked()
    }

    fn get_unlocked(
        &self,
        key: &WarmResourceKey,
    ) -> Result<Option<(WarmResourceReceipt, Vec<u8>)>, String> {
        let entry_id = key_fingerprint(key)?;
        let object_path = self.object_path(&entry_id);
        let metadata_path = self.metadata_path(&entry_id);
        if !object_path.is_file() || !metadata_path.is_file() {
            if object_path.exists() || metadata_path.exists() {
                remove_pair(&object_path, &metadata_path)?;
            }
            return Ok(None);
        }

        let receipt = match read_receipt(&metadata_path) {
            Ok(receipt) => receipt,
            Err(_) => {
                remove_pair(&object_path, &metadata_path)?;
                return Ok(None);
            }
        };
        let now = now_unix_seconds()?;
        if receipt.schema_version != RESOURCE_POOL_SCHEMA_VERSION
            || receipt.key != *key
            || now.saturating_sub(receipt.created_unix_seconds) > self.limits.max_age_seconds
        {
            remove_pair(&object_path, &metadata_path)?;
            return Ok(None);
        }

        let bytes = match fs::read(&object_path) {
            Ok(bytes) => bytes,
            Err(_) => {
                remove_pair(&object_path, &metadata_path)?;
                return Ok(None);
            }
        };
        if bytes.len() as u64 != receipt.byte_len || sha256(&bytes) != receipt.object_sha256 {
            remove_pair(&object_path, &metadata_path)?;
            return Ok(None);
        }
        Ok(Some((receipt, bytes)))
    }

    fn prune_unlocked(&self) -> Result<usize, String> {
        let now = now_unix_seconds()?;
        let mut entries = BTreeMap::<String, WarmResourceReceipt>::new();
        let directory = fs::read_dir(&self.root).map_err(|error| {
            format!(
                "failed to inspect warm verification resource pool {}: {error}",
                self.root.display()
            )
        })?;
        let mut removed = 0usize;

        for item in directory {
            let item =
                item.map_err(|error| format!("failed to inspect warm resource entry: {error}"))?;
            let path = item.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };

            if name.starts_with('.') && name.contains(".tmp-") {
                remove_file_if_present(&path)?;
                removed += 1;
                continue;
            }

            if let Some(entry_id) = name.strip_suffix(OBJECT_SUFFIX) {
                if !self.metadata_path(entry_id).is_file() {
                    remove_file_if_present(&path)?;
                    removed += 1;
                }
                continue;
            }

            let Some(entry_id) = name.strip_suffix(METADATA_SUFFIX) else {
                continue;
            };
            let object_path = self.object_path(entry_id);
            let receipt = read_receipt(&path);
            let valid = receipt.as_ref().is_ok_and(|receipt| {
                receipt.schema_version == RESOURCE_POOL_SCHEMA_VERSION
                    && now.saturating_sub(receipt.created_unix_seconds)
                        <= self.limits.max_age_seconds
                    && receipt.byte_len <= self.limits.max_bytes
                    && object_path.is_file()
            });
            if !valid {
                remove_pair(&object_path, &path)?;
                removed += 1;
                continue;
            }
            let Ok(receipt) = receipt else {
                continue;
            };
            if key_fingerprint(&receipt.key).as_deref() != Ok(entry_id) {
                remove_pair(&object_path, &path)?;
                removed += 1;
                continue;
            }
            entries.insert(entry_id.to_owned(), receipt);
        }

        let mut ordered = entries.into_iter().collect::<Vec<_>>();
        ordered.sort_by(|(left_id, left), (right_id, right)| {
            left.created_unix_seconds
                .cmp(&right.created_unix_seconds)
                .then_with(|| left_id.cmp(right_id))
        });
        let mut total_bytes = ordered
            .iter()
            .map(|(_, receipt)| receipt.byte_len)
            .sum::<u64>();
        let mut total_entries = ordered.len();
        for (entry_id, receipt) in ordered {
            if total_entries <= self.limits.max_entries && total_bytes <= self.limits.max_bytes {
                break;
            }
            remove_pair(&self.object_path(&entry_id), &self.metadata_path(&entry_id))?;
            total_entries = total_entries.saturating_sub(1);
            total_bytes = total_bytes.saturating_sub(receipt.byte_len);
            removed += 1;
        }
        Ok(removed)
    }

    fn lock_operations(&self) -> Result<std::sync::MutexGuard<'_, ()>, String> {
        self.operation_lock
            .lock()
            .map_err(|_| "warm verification resource pool lock is poisoned".to_owned())
    }

    fn object_path(&self, entry_id: &str) -> PathBuf {
        self.root.join(format!("{entry_id}{OBJECT_SUFFIX}"))
    }

    fn metadata_path(&self, entry_id: &str) -> PathBuf {
        self.root.join(format!("{entry_id}{METADATA_SUFFIX}"))
    }
}

fn key_fingerprint(key: &WarmResourceKey) -> Result<String, String> {
    let encoded = serde_json::to_vec(key)
        .map_err(|error| format!("failed to serialize warm resource key: {error}"))?;
    Ok(sha256(&encoded))
}

fn read_receipt(path: &Path) -> Result<WarmResourceReceipt, String> {
    let bytes = fs::read(path).map_err(|error| {
        format!(
            "failed to read warm resource metadata {}: {error}",
            path.display()
        )
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "failed to decode warm resource metadata {}: {error}",
            path.display()
        )
    })
}

fn write_new_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("invalid warm resource path {}", path.display()))?;
    let temporary = path.with_file_name(format!(".{file_name}.tmp-{}", Ulid::new()));
    fs::write(&temporary, bytes).map_err(|error| {
        format!(
            "failed to stage warm resource {}: {error}",
            temporary.display()
        )
    })?;
    if path.exists() {
        fs::remove_file(path).map_err(|error| {
            format!(
                "failed to replace warm resource {}: {error}",
                path.display()
            )
        })?;
    }
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!(
            "failed to publish warm resource {}: {error}",
            path.display()
        )
    })
}

fn remove_file_if_present(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "failed to remove warm resource {}: {error}",
            path.display()
        )),
    }
}

fn remove_pair(object_path: &Path, metadata_path: &Path) -> Result<(), String> {
    remove_file_if_present(object_path)?;
    remove_file_if_present(metadata_path)
}

fn now_unix_seconds() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| format!("system clock is before unix epoch: {error}"))
}

fn sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    fn key(kind: WarmResourceKind, input: &str) -> WarmResourceKey {
        WarmResourceKey {
            repository_fingerprint: "repo-a".to_owned(),
            branch_fingerprint: "branch-a".to_owned(),
            trust_domain: "trusted-local".to_owned(),
            resource_kind: kind,
            input_fingerprint: input.to_owned(),
            toolchain_fingerprint: "rust-1.92".to_owned(),
        }
    }

    fn pool(directory: &Path, max_entries: usize, max_bytes: u64) -> WarmResourcePool {
        WarmResourcePool::open(
            directory,
            WarmResourcePoolLimits {
                max_entries,
                max_bytes,
                max_age_seconds: 3_600,
            },
        )
        .expect("pool")
    }

    #[test]
    fn exact_provenance_key_reuses_immutable_resource() {
        let directory = tempfile::tempdir().expect("directory");
        let pool = pool(directory.path(), 4, 1024);
        let key = key(WarmResourceKind::DependencyCache, "lock-a");
        let receipt = pool.put(key.clone(), b"cached-dependencies").expect("put");
        let (reused, bytes) = pool.get(&key).expect("get").expect("resource");
        assert_eq!(reused, receipt);
        assert_eq!(bytes, b"cached-dependencies");
    }

    #[test]
    fn repository_branch_trust_and_toolchain_drift_cannot_reuse_resource() {
        let directory = tempfile::tempdir().expect("directory");
        let pool = pool(directory.path(), 8, 4096);
        let original = key(WarmResourceKind::BuildMetadata, "tree-a");
        pool.put(original.clone(), b"build-graph").expect("put");

        let mut repository = original.clone();
        repository.repository_fingerprint = "repo-b".to_owned();
        assert!(pool.get(&repository).expect("repository drift").is_none());
        let mut branch = original.clone();
        branch.branch_fingerprint = "branch-b".to_owned();
        assert!(pool.get(&branch).expect("branch drift").is_none());
        let mut trust = original.clone();
        trust.trust_domain = "untrusted".to_owned();
        assert!(pool.get(&trust).expect("trust drift").is_none());
        let mut toolchain = original;
        toolchain.toolchain_fingerprint = "rust-other".to_owned();
        assert!(pool.get(&toolchain).expect("toolchain drift").is_none());
    }

    #[test]
    fn corrupted_object_is_invalidated_instead_of_reused() {
        let directory = tempfile::tempdir().expect("directory");
        let pool = pool(directory.path(), 4, 1024);
        let key = key(WarmResourceKind::BrowserFixture, "browser-a");
        pool.put(key.clone(), b"browser-binary").expect("put");
        let entry_id = key_fingerprint(&key).expect("entry id");
        fs::write(pool.object_path(&entry_id), b"tampered").expect("corrupt");

        assert!(pool.get(&key).expect("corruption handled").is_none());
        assert!(!pool.object_path(&entry_id).exists());
        assert!(!pool.metadata_path(&entry_id).exists());
    }

    #[test]
    fn entry_and_byte_limits_evict_resources_deterministically() {
        let directory = tempfile::tempdir().expect("directory");
        let pool = pool(directory.path(), 2, 8);
        let first = key(WarmResourceKind::Worktree, "a");
        let second = key(WarmResourceKind::Worktree, "b");
        let third = key(WarmResourceKind::Worktree, "c");
        pool.put(first.clone(), b"1111").expect("first");
        pool.put(second.clone(), b"2222").expect("second");
        let _ = pool.put(third.clone(), b"3333");

        let present = [&first, &second, &third]
            .into_iter()
            .filter(|key| pool.get(key).expect("lookup").is_some())
            .count();
        assert_eq!(present, 2);
    }

    #[test]
    fn incomplete_pairs_are_cleaned_and_never_reused() {
        let directory = tempfile::tempdir().expect("directory");
        let pool = pool(directory.path(), 4, 1024);
        let key = key(WarmResourceKind::Sidecar, "sidecar-a");
        let entry_id = key_fingerprint(&key).expect("entry id");
        fs::write(pool.object_path(&entry_id), b"orphan").expect("orphan object");

        assert_eq!(pool.prune().expect("prune orphan"), 1);
        assert!(!pool.object_path(&entry_id).exists());
    }

    #[test]
    fn prune_removes_stale_temporary_files() {
        let directory = tempfile::tempdir().expect("directory");
        let pool = pool(directory.path(), 4, 1024);
        let temporary = directory.path().join(".orphan.bin.tmp-01H00000000000000000000000");
        fs::write(&temporary, b"orphan").expect("temporary");

        assert_eq!(pool.prune().expect("prune temporary"), 1);
        assert!(!temporary.exists());
    }

    #[test]
    fn cloned_pool_serializes_same_key_publication_and_reads() {
        let directory = tempfile::tempdir().expect("directory");
        let pool = pool(directory.path(), 4, 1024);
        let key = key(WarmResourceKind::DependencyCache, "concurrent");
        let writer = pool.clone();
        let reader = pool.clone();
        let writer_key = key.clone();
        let reader_key = key.clone();

        let write = thread::spawn(move || writer.put(writer_key, b"shared-resource"));
        let read = thread::spawn(move || {
            for _ in 0..64 {
                if let Some(resource) = reader.get(&reader_key).expect("read") {
                    return Some(resource);
                }
                thread::yield_now();
            }
            None
        });

        let written = write.join().expect("writer").expect("put");
        let observed = read.join().expect("reader");
        if let Some((receipt, bytes)) = observed {
            assert_eq!(receipt, written);
            assert_eq!(bytes, b"shared-resource");
        }
        assert_eq!(
            pool.get(&key).expect("final read").expect("resource").1,
            b"shared-resource"
        );
    }
}
