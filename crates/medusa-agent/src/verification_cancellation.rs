use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, atomic::AtomicBool},
};

#[derive(Debug)]
struct ActiveCancellation {
    token: Arc<AtomicBool>,
    registrations: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct VerificationRuntimeMetrics {
    pub command_waves: u64,
    pub command_checks_executed: u64,
    pub command_queue_duration_ms: u64,
    pub command_serial_execution_ms: u64,
    pub command_wall_duration_ms: u64,
    pub command_overlap_ms: u64,
}

impl VerificationRuntimeMetrics {
    fn record_command_wave(
        &mut self,
        checks_executed: usize,
        queue_duration_ms: u64,
        serial_execution_ms: u64,
        wall_duration_ms: u64,
    ) {
        self.command_waves = self.command_waves.saturating_add(1);
        self.command_checks_executed = self
            .command_checks_executed
            .saturating_add(checks_executed as u64);
        self.command_queue_duration_ms = self
            .command_queue_duration_ms
            .saturating_add(queue_duration_ms);
        self.command_serial_execution_ms = self
            .command_serial_execution_ms
            .saturating_add(serial_execution_ms);
        self.command_wall_duration_ms = self
            .command_wall_duration_ms
            .saturating_add(wall_duration_ms);
        self.command_overlap_ms = self
            .command_overlap_ms
            .saturating_add(serial_execution_ms.saturating_sub(wall_duration_ms));
    }
}

static ACTIVE: OnceLock<Mutex<BTreeMap<PathBuf, ActiveCancellation>>> = OnceLock::new();
static RUNTIME_METRICS: OnceLock<Mutex<BTreeMap<PathBuf, VerificationRuntimeMetrics>>> =
    OnceLock::new();

fn active() -> &'static Mutex<BTreeMap<PathBuf, ActiveCancellation>> {
    ACTIVE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn runtime_metrics() -> &'static Mutex<BTreeMap<PathBuf, VerificationRuntimeMetrics>> {
    RUNTIME_METRICS.get_or_init(|| Mutex::new(BTreeMap::new()))
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
    let mut active = active()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = active
        .entry(root.clone())
        .or_insert_with(|| ActiveCancellation {
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
    let active = active()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    active
        .iter()
        .filter(|(root, _)| path.starts_with(root))
        .max_by_key(|(root, _)| root.components().count())
        .map(|(_, entry)| Arc::clone(&entry.token))
}

pub(crate) fn record_command_wave(
    repo: &Path,
    checks_executed: usize,
    queue_duration_ms: u64,
    serial_execution_ms: u64,
    wall_duration_ms: u64,
) {
    if checks_executed == 0 {
        return;
    }
    if let Ok(mut metrics) = runtime_metrics().lock() {
        metrics
            .entry(normalized(repo))
            .or_default()
            .record_command_wave(
                checks_executed,
                queue_duration_ms,
                serial_execution_ms,
                wall_duration_ms,
            );
    }
}

// Drains repository-scoped verification runtime metrics after an authority run.
pub(crate) fn take_runtime_metrics(repo: &Path) -> VerificationRuntimeMetrics {
    runtime_metrics()
        .lock()
        .ok()
        .and_then(|mut metrics| metrics.remove(&normalized(repo)))
        .unwrap_or_default()
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

    #[test]
    fn runtime_metrics_record_command_overlap_and_drain_per_repository() {
        let directory = tempfile::tempdir().expect("repository");
        let mut metrics = VerificationRuntimeMetrics::default();
        metrics.record_command_wave(2, 7, 120, 70);
        assert_eq!(metrics.command_waves, 1);
        assert_eq!(metrics.command_checks_executed, 2);
        assert_eq!(metrics.command_queue_duration_ms, 7);
        assert_eq!(metrics.command_serial_execution_ms, 120);
        assert_eq!(metrics.command_wall_duration_ms, 70);
        assert_eq!(metrics.command_overlap_ms, 50);

        record_command_wave(directory.path(), 2, 7, 120, 70);
        assert_eq!(take_runtime_metrics(directory.path()), metrics);
        assert_eq!(
            take_runtime_metrics(directory.path()),
            VerificationRuntimeMetrics::default()
        );
    }

    #[test]
    fn empty_internal_only_wave_does_not_claim_command_metrics() {
        let directory = tempfile::tempdir().expect("repository");
        record_command_wave(directory.path(), 0, 0, 0, 25);
        assert_eq!(
            take_runtime_metrics(directory.path()),
            VerificationRuntimeMetrics::default()
        );
    }
}
