use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, OnceLock},
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
    use std::{sync::mpsc, thread, time::Duration};

    use super::*;

    #[test]
    fn aliases_to_same_repository_serialize() {
        let directory = tempfile::tempdir().expect("tempdir");
        let alias = directory.path().join("nested").join("..");
        fs::create_dir_all(directory.path().join("nested")).expect("nested");
        let first = lock(directory.path());
        let path = alias;
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _second = lock(&path);
            sender.send(()).expect("send");
        });
        assert!(receiver.recv_timeout(Duration::from_millis(50)).is_err());
        drop(first);
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("second lock");
        worker.join().expect("worker");
    }
}
