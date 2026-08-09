use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::AtomicBool,
    },
};

#[derive(Debug)]
struct ActiveCancellation {
    token: Arc<AtomicBool>,
    registrations: usize,
}

static ACTIVE: OnceLock<Mutex<BTreeMap<PathBuf, ActiveCancellation>>> = OnceLock::new();

fn active() -> &'static Mutex<BTreeMap<PathBuf, ActiveCancellation>> {
    ACTIVE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn normalized(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[derive(Debug)]
pub(crate) struct VerificationCancellationRegistration {
    root: PathBuf,
    token: Arc<AtomicBool>,
}

impl VerificationCancellationRegistration {
    pub(crate) fn token(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.token)
    }
}

impl Drop for VerificationCancellationRegistration {
    fn drop(&mut self) {
        let Ok(mut active) = active().lock() else {
            return;
        };
        let remove = if let Some(entry) = active.get_mut(&self.root) {
            entry.registrations = entry.registrations.saturating_sub(1);
            entry.registrations == 0
        } else {
            false
        };
        if remove {
            active.remove(&self.root);
        }
    }
}

pub(crate) fn register_verification_cancellation(
    repo: &Path,
) -> VerificationCancellationRegistration {
    let root = normalized(repo);
    let mut active = active().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = active.entry(root.clone()).or_insert_with(|| ActiveCancellation {
        token: Arc::new(AtomicBool::new(false)),
        registrations: 0,
    });
    entry.registrations += 1;
    VerificationCancellationRegistration {
        root,
        token: Arc::clone(&entry.token),
    }
}

pub(crate) fn active_verification_cancellation(path: &Path) -> Option<Arc<AtomicBool>> {
    let path = normalized(path);
    let active = active().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    active
        .iter()
        .filter(|(root, _)| path.starts_with(root))
        .max_by_key(|(root, _)| root.components().count())
        .map(|(_, entry)| Arc::clone(&entry.token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_is_visible_to_repository_descendants_and_removed_on_drop() {
        let directory = tempfile::tempdir().expect("repository");
        let nested = directory.path().join("nested");
        std::fs::create_dir_all(&nested).expect("nested");

        let registration = register_verification_cancellation(directory.path());
        let nested_token = active_verification_cancellation(&nested).expect("active token");
        assert!(Arc::ptr_eq(&registration.token(), &nested_token));

        drop(registration);
        assert!(active_verification_cancellation(&nested).is_none());
    }

    #[test]
    fn concurrent_repository_registrations_share_one_cancellation_token() {
        let directory = tempfile::tempdir().expect("repository");
        let first = register_verification_cancellation(directory.path());
        let second = register_verification_cancellation(directory.path());
        assert!(Arc::ptr_eq(&first.token(), &second.token()));

        drop(first);
        assert!(active_verification_cancellation(directory.path()).is_some());
        drop(second);
        assert!(active_verification_cancellation(directory.path()).is_none());
    }
}
