use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{ProviderHealth, ProviderRouteProfile};

const STORE_SCHEMA_VERSION: u16 = 1;
const STORE_FILE_NAME: &str = "provider-runtime.sqlite3";

#[derive(Clone)]
pub struct ProviderHealthStore {
    backend: StoreBackend,
    route_keys: Arc<Vec<String>>,
    profiles: Arc<Vec<ProviderRouteProfile>>,
}

#[derive(Clone)]
enum StoreBackend {
    Memory(Arc<Mutex<ProviderRuntimeState>>),
    Sqlite(PathBuf),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ProviderRuntimeState {
    schema_version: u16,
    routes: BTreeMap<String, ProviderHealth>,
    last_completed_route: Option<String>,
    cache_hits: u64,
    last_execution: Option<Value>,
}

impl Default for ProviderRuntimeState {
    fn default() -> Self {
        Self {
            schema_version: STORE_SCHEMA_VERSION,
            routes: BTreeMap::new(),
            last_completed_route: None,
            cache_hits: 0,
            last_execution: None,
        }
    }
}

impl ProviderHealthStore {
    #[must_use]
    pub fn in_memory(profiles: &[ProviderRouteProfile]) -> Self {
        Self {
            backend: StoreBackend::Memory(Arc::new(Mutex::new(ProviderRuntimeState::default()))),
            route_keys: Arc::new(profiles.iter().map(route_key).collect()),
            profiles: Arc::new(profiles.to_vec()),
        }
    }

    pub fn for_repository(
        repository: &Path,
        profiles: &[ProviderRouteProfile],
    ) -> MedusaResult<Self> {
        let state_directory = repository.join(".medusa");
        fs::create_dir_all(&state_directory).map_err(store_io_error)?;
        let store = Self {
            backend: StoreBackend::Sqlite(state_directory.join(STORE_FILE_NAME)),
            route_keys: Arc::new(profiles.iter().map(route_key).collect()),
            profiles: Arc::new(profiles.to_vec()),
        };
        store.with_state(|_| ())?;
        Ok(store)
    }

    pub fn health(&self) -> MedusaResult<Vec<ProviderHealth>> {
        self.read_state(|state| {
            self.route_keys
                .iter()
                .map(|key| state.routes.get(key).cloned().unwrap_or_default())
                .collect()
        })
    }

    pub fn last_completed_provider(&self) -> MedusaResult<Option<usize>> {
        self.read_state(|state| {
            state
                .last_completed_route
                .as_ref()
                .and_then(|key| self.route_keys.iter().position(|candidate| candidate == key))
        })
    }

    pub fn cache_hits(&self) -> MedusaResult<u64> {
        self.read_state(|state| state.cache_hits)
    }

    pub fn execution_status(&self) -> MedusaResult<Option<Value>> {
        self.read_state(|state| state.last_execution.clone())
    }

    pub fn record_attempt(&self, index: usize) -> MedusaResult<()> {
        self.update_health(index, |entry| {
            entry.attempts = entry.attempts.saturating_add(1);
        })
    }

    pub fn record_error(&self, index: usize, error: &MedusaError) -> MedusaResult<()> {
        self.update_health(index, |entry| {
            entry.last_error = Some(error.to_string());
        })
    }

    pub fn record_retry(&self, index: usize, delay_ms: u64) -> MedusaResult<()> {
        self.update_health(index, |entry| {
            entry.retries = entry.retries.saturating_add(1);
            entry.last_delay_ms = Some(delay_ms);
        })
    }

    pub fn record_failover(&self, index: usize) -> MedusaResult<()> {
        self.update_health(index, |entry| {
            entry.failovers = entry.failovers.saturating_add(1);
        })
    }

    pub fn record_success(&self, index: usize) -> MedusaResult<()> {
        let key = self.key(index)?.to_owned();
        let profile = self.profile(index)?.clone();
        self.with_state(|state| {
            let health = {
                let entry = state.routes.entry(key.clone()).or_default();
                entry.successes = entry.successes.saturating_add(1);
                entry.last_error = None;
                entry.last_delay_ms = None;
                entry.clone()
            };
            state.last_completed_route = Some(key);
            state.last_execution = Some(execution_snapshot(
                index,
                &profile,
                false,
                state.cache_hits,
                &health,
            ));
        })
    }

    pub fn record_cache_hit(&self) -> MedusaResult<()> {
        self.with_state(|state| {
            state.cache_hits = state.cache_hits.saturating_add(1);
            let Some(route_key) = state.last_completed_route.clone() else {
                return;
            };
            let Some(index) = self
                .route_keys
                .iter()
                .position(|candidate| candidate == &route_key)
            else {
                return;
            };
            let Some(profile) = self.profiles.get(index) else {
                return;
            };
            let health = state.routes.get(&route_key).cloned().unwrap_or_default();
            state.last_execution = Some(execution_snapshot(
                index,
                profile,
                true,
                state.cache_hits,
                &health,
            ));
        })
    }

    fn update_health(
        &self,
        index: usize,
        update: impl FnOnce(&mut ProviderHealth),
    ) -> MedusaResult<()> {
        let key = self.key(index)?.to_owned();
        self.with_state(|state| update(state.routes.entry(key).or_default()))
    }

    fn key(&self, index: usize) -> MedusaResult<&str> {
        self.route_keys.get(index).map(String::as_str).ok_or_else(|| {
            store_error(format!("provider route index {index} is not configured"))
        })
    }

    fn profile(&self, index: usize) -> MedusaResult<&ProviderRouteProfile> {
        self.profiles.get(index).ok_or_else(|| {
            store_error(format!("provider route profile {index} is not configured"))
        })
    }

    fn read_state<T>(&self, read: impl FnOnce(&ProviderRuntimeState) -> T) -> MedusaResult<T> {
        self.with_state(|state| read(state))
    }

    fn with_state<T>(&self, update: impl FnOnce(&mut ProviderRuntimeState) -> T) -> MedusaResult<T> {
        match &self.backend {
            StoreBackend::Memory(state) => {
                let mut state = state
                    .lock()
                    .map_err(|_| store_error("provider runtime state lock is poisoned"))?;
                Ok(update(&mut state))
            }
            StoreBackend::Sqlite(path) => {
                let mut connection = open_connection(path)?;
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(sqlite_error)?;
                let payload = transaction
                    .query_row(
                        "SELECT payload FROM provider_runtime_state WHERE singleton = 1",
                        [],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(sqlite_error)?;
                let mut state = payload.map_or_else(
                    || Ok(ProviderRuntimeState::default()),
                    |payload| serde_json::from_str(&payload).map_err(serialization_error),
                )?;
                if state.schema_version != STORE_SCHEMA_VERSION {
                    return Err(store_error(format!(
                        "unsupported provider runtime state schema {}",
                        state.schema_version
                    )));
                }
                let result = update(&mut state);
                let payload = serde_json::to_string(&state).map_err(serialization_error)?;
                transaction
                    .execute(
                        "INSERT INTO provider_runtime_state(singleton, payload) VALUES(1, ?1) \
                         ON CONFLICT(singleton) DO UPDATE SET payload = excluded.payload",
                        params![payload],
                    )
                    .map_err(sqlite_error)?;
                transaction.commit().map_err(sqlite_error)?;
                Ok(result)
            }
        }
    }
}

fn open_connection(path: &Path) -> MedusaResult<Connection> {
    let connection = Connection::open(path).map_err(sqlite_error)?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(sqlite_error)?;
    connection
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;
             CREATE TABLE IF NOT EXISTS provider_runtime_state (
               singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
               payload TEXT NOT NULL
             );",
        )
        .map_err(sqlite_error)?;
    Ok(connection)
}

fn route_key(profile: &ProviderRouteProfile) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        profile.id,
        profile.provider,
        profile.model,
        profile.protocol,
        profile.endpoint.as_deref().unwrap_or_default(),
        profile.auth_source,
    )
}

fn execution_snapshot(
    index: usize,
    route: &ProviderRouteProfile,
    cache_hit: bool,
    cache_hits: u64,
    health: &ProviderHealth,
) -> Value {
    json!({
        "provider_index": index,
        "route_id": route.id,
        "provider": route.provider,
        "model": route.model,
        "protocol": route.protocol,
        "endpoint": route.endpoint,
        "auth_source": route.auth_source,
        "tool_calling": route.tool_calling,
        "streaming": route.streaming,
        "cache_hit": cache_hit,
        "cache_hits": cache_hits,
        "attempts": health.attempts,
        "retries": health.retries,
        "failovers": health.failovers,
        "successes": health.successes,
        "last_delay_ms": health.last_delay_ms,
        "last_error": health.last_error,
    })
}

fn store_io_error(error: std::io::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Environment,
        format!("provider runtime state is unavailable: {error}"),
    )
}

fn sqlite_error(error: rusqlite::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Execution,
        format!("provider runtime state transaction failed: {error}"),
    )
}

fn serialization_error(error: serde_json::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        format!("provider runtime state is invalid: {error}"),
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
    fn repository_store_is_shared_across_instances() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let profiles = vec![profile("gpt-5")];
        let first = ProviderHealthStore::for_repository(directory.path(), &profiles)
            .expect("first store");
        first.record_attempt(0).expect("record attempt");
        first.record_success(0).expect("record success");

        let second = ProviderHealthStore::for_repository(directory.path(), &profiles)
            .expect("second store");
        assert_eq!(second.health().expect("health")[0].attempts, 1);
        assert_eq!(second.health().expect("health")[0].successes, 1);
        assert_eq!(second.last_completed_provider().expect("last provider"), Some(0));
        assert_eq!(
            second.execution_status().expect("execution status").expect("snapshot")["model"],
            json!("gpt-5")
        );
    }

    #[test]
    fn changed_route_profile_does_not_reuse_old_health() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let first = ProviderHealthStore::for_repository(directory.path(), &[profile("gpt-5")])
            .expect("first store");
        first.record_attempt(0).expect("record attempt");

        let second = ProviderHealthStore::for_repository(directory.path(), &[profile("gpt-6")])
            .expect("second store");
        assert_eq!(second.health().expect("health"), vec![ProviderHealth::default()]);
    }
}
