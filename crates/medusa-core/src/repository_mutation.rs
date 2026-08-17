use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, OnceLock},
};

static REPOSITORY_LOCKS: OnceLock<Mutex<BTreeMap<PathBuf, &'static Mutex<()>>>> = OnceLock::new();

/// Process-wide repository mutation guard shared by every Medusa mutation crate.
///
/// Repository paths are canonicalized before lookup so independently constructed paths to the
/// same repository contend on one lock. The lock objects are intentionally leaked for process
/// lifetime: repository mutation identities are few and stable, while returning a `'static` guard
/// lets callers hold the boundary without exposing the registry mutex.
pub struct RepositoryMutationGuard {
    _guard: MutexGuard<'static, ()>,
}

#[must_use]
pub fn lock(repo: &Path) -> RepositoryMutationGuard {
    let key = fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let registry = REPOSITORY_LOCKS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let repository_mutex = {
        let mut registry = registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *registry.entry(key).or_insert_with(|| {
            let mutex = Box::new(Mutex::new(()));
            Box::leak(mutex)
        })
    };
    RepositoryMutationGuard {
        _guard: repository_mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::mpsc,
        thread,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use super::*;

    #[test]
    fn aliases_to_same_repository_serialize() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("medusa-core-lock-{nonce}"));
        fs::create_dir_all(directory.join("nested")).expect("fixture");
        let alias = directory.join("nested").join("..");
        let first = lock(&directory);
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _second = lock(&alias);
            sender.send(()).expect("send");
        });
        assert!(receiver.recv_timeout(Duration::from_millis(50)).is_err());
        drop(first);
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("second lock");
        worker.join().expect("worker");
        fs::remove_dir_all(directory).expect("cleanup");
    }
}
