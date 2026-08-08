use std::{
    collections::BTreeMap,
    fs,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime},
};

use medusa_config::ProviderProfileCatalog;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};

use crate::{ProviderRouteProfile, RouteLatencyStats, Usage};

const STORE_SCHEMA_VERSION: u16 = 1;
const STORE_FILE_NAME: &str = "provider-route-latency.json";
const LOCK_FILE_NAME: &str = ".provider-route-latency.lock";
const LOCK_RETRY_ATTEMPTS: usize = 200;
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(10);
const STALE_LOCK_AGE: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct ProviderRouteLatencyStore {
    backend: StoreBackend,
    route_keys: Arc<Vec<String>>,
}

#[derive(Clone)]
enum StoreBackend {
    Memory(Arc<Mutex<RouteLatencyState>>),
    File {
        state_path: PathBuf,
        lock_path: PathBuf,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RouteLatencyState {
    schema_version: u16,
    routes: BTreeMap<String, RouteLatencyStats>,
}

impl Default for RouteLatencyState {
    fn default() -> Self {
        Self {
            schema_version: STORE_SCHEMA_VERSION,
            routes: BTreeMap::new(),
        }
    }
}

impl ProviderRouteLatencyStore {
    #[must_use]
    pub fn in_memory(profiles: &[ProviderRouteProfile]) -> Self {
        Self {
            backend: StoreBackend::Memory(Arc::new(Mutex::new(RouteLatencyState::default()))),
            route_keys: Arc::new(profiles.iter().map(route_key).collect()),
        }
    }

    pub fn for_user(profiles: &[ProviderRouteProfile]) -> MedusaResult<Self> {
        let root = ProviderProfileCatalog::user()?.root().to_path_buf();
        Self::at(root, profiles)
    }

    pub fn at(root: impl Into<PathBuf>, profiles: &[ProviderRouteProfile]) -> MedusaResult<Self> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(store_io_error)?;
        let store = Self {
            backend: StoreBackend::File {
                state_path: root.join(STORE_FILE_NAME),
                lock_path: root.join(LOCK_FILE_NAME),
            },
            route_keys: Arc::new(profiles.iter().map(route_key).collect()),
        };
        store.with_state(|_| ())?;
        Ok(store)
    }

    pub fn stats(&self) -> MedusaResult<Vec<RouteLatencyStats>> {
        self.read_state(|state| {
            self.route_keys
                .iter()
                .map(|key| state.routes.get(key).copied().unwrap_or_default())
                .collect()
        })
    }

    pub fn record_success(&self, index: usize, duration_ms: u64, usage: Usage) -> MedusaResult<()> {
        self.record_success_with_first_token(index, duration_ms, None, usage)
    }

    pub fn record_success_with_first_token(
        &self,
        index: usize,
        duration_ms: u64,
        first_token_ms: Option<u64>,
        usage: Usage,
    ) -> MedusaResult<()> {
        self.update(index, |stats| {
            stats.samples = stats.samples.saturating_add(1);
            stats.successes = stats.successes.saturating_add(1);
            stats.total_duration_ms = stats.total_duration_ms.saturating_add(duration_ms);
            if let Some(first_token_ms) = first_token_ms {
                stats.first_token_samples = stats.first_token_samples.saturating_add(1);
                stats.total_first_token_ms =
                    stats.total_first_token_ms.saturating_add(first_token_ms);
            }
            stats.input_tokens = stats.input_tokens.saturating_add(usage.input_tokens);
            stats.cached_input_tokens = stats
                .cached_input_tokens
                .saturating_add(usage.cache_read_input_tokens);
            if let Some(first_token_ms) = first_token_ms {
                stats.output_tokens = stats.output_tokens.saturating_add(usage.output_tokens);
                let generation_ms = duration_ms.saturating_sub(first_token_ms);
                stats.generation_total_ms = stats.generation_total_ms.saturating_add(generation_ms);
            }
        })
    }

    pub fn record_retry_attempt(&self, index: usize) -> MedusaResult<()> {
        self.update(index, |stats| {
            stats.retry_attempts = stats.retry_attempts.saturating_add(1);
        })
    }

    pub fn record_retry_recovery(&self, index: usize) -> MedusaResult<()> {
        self.update(index, |stats| {
            stats.retry_recoveries = stats.retry_recoveries.saturating_add(1);
        })
    }

    pub fn record_failure(&self, index: usize, duration_ms: u64) -> MedusaResult<()> {
        self.update(index, |stats| {
            stats.samples = stats.samples.saturating_add(1);
            stats.failures = stats.failures.saturating_add(1);
            stats.total_duration_ms = stats.total_duration_ms.saturating_add(duration_ms);
        })
    }

    pub fn record_cancellation(&self, index: usize, duration_ms: u64) -> MedusaResult<()> {
        self.update(index, |stats| {
            stats.cancellation_samples = stats.cancellation_samples.saturating_add(1);
            stats.cancellation_total_ms = stats.cancellation_total_ms.saturating_add(duration_ms);
        })
    }

    fn update(
        &self,
        index: usize,
        update: impl FnOnce(&mut RouteLatencyStats),
    ) -> MedusaResult<()> {
        let key = self
            .route_keys
            .get(index)
            .ok_or_else(|| store_error(format!("provider route index {index} is not configured")))?
            .to_owned();
        self.with_state(|state| update(state.routes.entry(key).or_default()))
    }

    fn read_state<T>(&self, read: impl FnOnce(&RouteLatencyState) -> T) -> MedusaResult<T> {
        self.with_state(|state| read(state))
    }

    fn with_state<T>(&self, update: impl FnOnce(&mut RouteLatencyState) -> T) -> MedusaResult<T> {
        match &self.backend {
            StoreBackend::Memory(state) => {
                let mut state = state
                    .lock()
                    .map_err(|_| store_error("provider route latency state lock is poisoned"))?;
                Ok(update(&mut state))
            }
            StoreBackend::File {
                state_path,
                lock_path,
            } => {
                let _guard = FileLock::acquire(lock_path)?;
                let mut state = load_state(state_path)?;
                let result = update(&mut state);
                let bytes = serde_json::to_vec_pretty(&state).map_err(serialization_error)?;
                atomic_write(state_path, &bytes)?;
                Ok(result)
            }
        }
    }
}

struct FileLock {
    path: PathBuf,
}

impl FileLock {
    fn acquire(path: &Path) -> MedusaResult<Self> {
        for _ in 0..LOCK_RETRY_ATTEMPTS {
            match OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(mut file) => {
                    writeln!(file, "pid={}", std::process::id()).map_err(store_io_error)?;
                    file.sync_all().map_err(store_io_error)?;
                    return Ok(Self {
                        path: path.to_path_buf(),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if lock_is_stale(path) {
                        let _ = fs::remove_file(path);
                    }
                    thread::sleep(LOCK_RETRY_DELAY);
                }
                Err(error) => return Err(store_io_error(error)),
            }
        }
        Err(store_error(format!(
            "provider route latency state is busy; could not acquire {}",
            path.display()
        )))
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        sync_parent(&self.path);
    }
}

fn load_state(path: &Path) -> MedusaResult<RouteLatencyState> {
    if !path.exists() {
        return Ok(RouteLatencyState::default());
    }
    let bytes = fs::read(path).map_err(store_io_error)?;
    let state: RouteLatencyState = serde_json::from_slice(&bytes).map_err(serialization_error)?;
    if state.schema_version != STORE_SCHEMA_VERSION {
        return Err(store_error(format!(
            "unsupported provider route latency schema {}",
            state.schema_version
        )));
    }
    Ok(state)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> MedusaResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| store_error("provider route latency path has no parent"))?;
    fs::create_dir_all(parent).map_err(store_io_error)?;
    let temporary = path.with_extension("json.tmp");
    let mut file = fs::File::create(&temporary).map_err(store_io_error)?;
    file.write_all(bytes).map_err(store_io_error)?;
    file.sync_all().map_err(store_io_error)?;
    fs::rename(&temporary, path).map_err(store_io_error)?;
    sync_parent(path);
    Ok(())
}

fn sync_parent(path: &Path) {
    #[cfg(unix)]
    if let Some(parent) = path.parent()
        && let Ok(directory) = fs::File::open(parent)
    {
        let _ = directory.sync_all();
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn lock_is_stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age >= STALE_LOCK_AGE)
}

fn route_key(profile: &ProviderRouteProfile) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        profile.id,
        profile.provider,
        profile.model,
        profile.protocol,
        profile.endpoint.as_deref().unwrap_or_default(),
        profile.auth_source,
        profile.streaming,
    )
}

fn store_io_error(error: std::io::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Environment,
        format!("provider route latency state is unavailable: {error}"),
    )
}

fn serialization_error(error: serde_json::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        format!("provider route latency state is invalid: {error}"),
    )
}

fn store_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Execution,
        message.into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RouteRetryPolicy;

    fn profile(model: &str) -> ProviderRouteProfile {
        ProviderRouteProfile {
            id: "primary".to_owned(),
            provider: "openai".to_owned(),
            model: model.to_owned(),
            protocol: "openai".to_owned(),
            endpoint: Some("https://example.invalid/v1".to_owned()),
            auth_source: "api-key".to_owned(),
            tool_calling: true,
            streaming: false,
            retry: RouteRetryPolicy::default(),
        }
    }

    #[test]
    fn observations_are_shared_across_instances_and_scoped_to_route_identity() {
        let directory = tempfile::tempdir().expect("temporary profile root");
        let profiles = vec![profile("gpt-5")];
        let first = ProviderRouteLatencyStore::at(directory.path(), &profiles).expect("first");
        first
            .record_success(
                0,
                120,
                Usage {
                    input_tokens: 100,
                    cache_read_input_tokens: 90,
                    ..Usage::default()
                },
            )
            .expect("record success");

        let second = ProviderRouteLatencyStore::at(directory.path(), &profiles).expect("second");
        let stats = second.stats().expect("stats")[0];
        assert_eq!(stats.samples, 1);
        assert_eq!(stats.successes, 1);
        assert_eq!(stats.total_duration_ms, 120);
        assert_eq!(stats.cache_reuse_milli(), 900);
        assert_eq!(stats.output_tokens, 0);
        assert_eq!(stats.generation_total_ms, 0);

        let changed = ProviderRouteLatencyStore::at(directory.path(), &[profile("gpt-6")])
            .expect("changed route");
        assert_eq!(
            changed.stats().expect("changed stats"),
            vec![RouteLatencyStats::default()]
        );
    }
}
