use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

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

fn runtime_metrics_registry() -> &'static Mutex<BTreeMap<PathBuf, VerificationRuntimeMetrics>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<PathBuf, VerificationRuntimeMetrics>>> =
        OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn repository_key(repo: &Path) -> PathBuf {
    repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf())
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
    if let Ok(mut registry) = runtime_metrics_registry().lock() {
        registry
            .entry(repository_key(repo))
            .or_default()
            .record_command_wave(
                checks_executed,
                queue_duration_ms,
                serial_execution_ms,
                wall_duration_ms,
            );
    }
}

pub(crate) fn take_runtime_metrics(repo: &Path) -> VerificationRuntimeMetrics {
    runtime_metrics_registry()
        .lock()
        .ok()
        .and_then(|mut registry| registry.remove(&repository_key(repo)))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

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
