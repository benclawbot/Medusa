use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

use crate::DaemonPaths;

static LOG_GUARD: OnceLock<Mutex<Option<WorkerGuard>>> = OnceLock::new();

#[derive(Debug, thiserror::Error)]
pub enum ObservabilityError {
    #[error("failed to create observability directory {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to install global tracing subscriber: {0}")]
    Subscriber(String),
    #[error("observability guard lock is poisoned")]
    GuardPoisoned,
}

/// Installs the process-wide JSON logger and keeps its non-blocking worker alive.
pub fn initialize_observability(repo: &Path) -> Result<(), ObservabilityError> {
    let log_directory = DaemonPaths::for_repo(repo).directory;
    fs::create_dir_all(&log_directory).map_err(|source| ObservabilityError::Io {
        path: log_directory.clone(),
        source,
    })?;

    let appender = tracing_appender::rolling::daily(&log_directory, "medusa.jsonl");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_env("MEDUSA_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_ansi(false)
        .with_env_filter(filter)
        .with_writer(writer)
        .finish();

    tracing::subscriber::set_global_default(subscriber)
        .map_err(|error| ObservabilityError::Subscriber(error.to_string()))?;

    let slot = LOG_GUARD.get_or_init(|| Mutex::new(None));
    *slot.lock().map_err(|_| ObservabilityError::GuardPoisoned)? = Some(guard);
    tracing::info!(log_directory = %log_directory.display(), "observability initialized");
    Ok(())
}
