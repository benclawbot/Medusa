//! Typed composition contracts for non-authority runtime services.
//!
//! Service providers can vary behind this seam, but they never become runtime authorities. The
//! registry owns admission, generation binding, lifecycle, and disposal; a provider only receives
//! an already-admitted request and a runtime-owned cancellation flag.

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use serde_json::json;

    use super::*;

    struct EchoProvider {
        started: AtomicBool,
        stopped: AtomicBool,
    }

    impl EchoProvider {
        fn new() -> Self {
            Self {
                started: AtomicBool::new(false),
                stopped: AtomicBool::new(false),
            }
        }
    }

    impl ServiceProvider for EchoProvider {
        fn descriptor(&self) -> ServiceProviderDescriptor {
            ServiceProviderDescriptor::new(
                "repository-search",
                "echo-search",
                "1.0.0",
                ServiceCapabilityClass::RepositorySearch,
                "config-fingerprint",
                "search-request-v1",
                "search-response-v1",
                ServiceAuthority::ReadOnly,
                vec![ServiceBoundary::RuntimeOwnedFilesystem],
                ServiceConcurrency::Concurrent,
                ServiceCancellation::Cooperative,
            )
            .expect("descriptor")
        }

        fn start(&self, _generation: u64) -> Result<(), ServiceProviderError> {
            self.started.store(true, Ordering::Release);
            Ok(())
        }

        fn stop(&self) -> Result<(), ServiceProviderError> {
            self.stopped.store(true, Ordering::Release);
            Ok(())
        }

        fn health(&self) -> ServiceProviderHealth {
            ServiceProviderHealth::ready()
        }

        fn execute(
            &self,
            request: &ServiceProviderRequest,
            cancel: &AtomicBool,
        ) -> Result<ServiceProviderResponse, ServiceProviderError> {
            if cancel.load(Ordering::Acquire) {
                return Err(ServiceProviderError::Cancelled);
            }
            Ok(ServiceProviderResponse::new(
                &self.descriptor(),
                request.generation,
                request.input.clone(),
            ))
        }
    }

    #[test]
    fn registry_admits_typed_provider_and_binds_generation() {
        let provider = std::sync::Arc::new(EchoProvider::new());
        let mut registry = ServiceProviderRegistry::new(7);
        registry.register(provider.clone()).expect("register");

        let lease = registry
            .admit("repository-search", "echo-search")
            .expect("admit");
        let response = lease
            .execute(json!({"query": "needle"}), &AtomicBool::new(false))
            .expect("execute");

        assert_eq!(lease.generation(), 7);
        assert_eq!(response.provider_id.as_str(), "echo-search");
        assert_eq!(response.generation, 7);
        assert!(provider.started.load(Ordering::Acquire));
        lease.close().expect("close");
        assert!(provider.stopped.load(Ordering::Acquire));
    }

    #[test]
    fn registry_rejects_duplicate_and_authority_replacement() {
        let mut registry = ServiceProviderRegistry::new(1);
        registry
            .register(std::sync::Arc::new(EchoProvider::new()))
            .expect("register");
        let duplicate = registry.register(std::sync::Arc::new(EchoProvider::new()));
        assert!(matches!(
            duplicate,
            Err(ServiceProviderError::Duplicate { .. })
        ));

        let fixed_authority = TestProvider::new(ServiceAuthority::FixedRuntimeAuthority);
        let error = registry
            .register(std::sync::Arc::new(fixed_authority))
            .expect_err("fixed authorities are not service-provider plugins");
        assert!(matches!(
            error,
            ServiceProviderError::AuthorityNotExtensible
        ));
    }

    #[test]
    fn lease_rejects_stale_generation_and_cancellation_before_provider_execution() {
        let provider = std::sync::Arc::new(EchoProvider::new());
        let mut registry = ServiceProviderRegistry::new(3);
        registry.register(provider).expect("register");
        let lease = registry
            .admit("repository-search", "echo-search")
            .expect("admit");

        let stale =
            lease.execute_with_generation(2, json!({"query": "needle"}), &AtomicBool::new(false));
        assert!(matches!(
            stale,
            Err(ServiceProviderError::StaleGeneration { .. })
        ));

        let cancelled = AtomicBool::new(true);
        let error = lease
            .execute(json!({"query": "needle"}), &cancelled)
            .expect_err("cancelled request");
        assert!(matches!(error, ServiceProviderError::Cancelled));
    }

    #[test]
    fn unregister_is_deterministic_and_waits_for_lease_disposal() {
        let provider = std::sync::Arc::new(EchoProvider::new());
        let mut registry = ServiceProviderRegistry::new(1);
        registry.register(provider).expect("register");
        let lease = registry
            .admit("repository-search", "echo-search")
            .expect("admit");
        assert!(matches!(
            registry.unregister("repository-search", "echo-search"),
            Err(ServiceProviderError::ActiveLeases { .. })
        ));
        lease.close().expect("close");
        registry
            .unregister("repository-search", "echo-search")
            .expect("unregister");
        assert!(matches!(
            registry.admit("repository-search", "echo-search"),
            Err(ServiceProviderError::NotRegistered { .. })
        ));
    }

    struct TestProvider(ServiceProviderDescriptor);

    impl TestProvider {
        fn new(authority: ServiceAuthority) -> Self {
            Self(
                ServiceProviderDescriptor::new(
                    "fixed-authority",
                    "test",
                    "1.0.0",
                    ServiceCapabilityClass::Diagnostics,
                    "config",
                    "request",
                    "response",
                    authority,
                    Vec::new(),
                    ServiceConcurrency::Concurrent,
                    ServiceCancellation::Cooperative,
                )
                .expect("descriptor"),
            )
        }
    }

    impl ServiceProvider for TestProvider {
        fn descriptor(&self) -> ServiceProviderDescriptor {
            self.0.clone()
        }

        fn start(&self, _generation: u64) -> Result<(), ServiceProviderError> {
            Ok(())
        }

        fn stop(&self) -> Result<(), ServiceProviderError> {
            Ok(())
        }

        fn health(&self) -> ServiceProviderHealth {
            ServiceProviderHealth::ready()
        }

        fn execute(
            &self,
            request: &ServiceProviderRequest,
            _cancel: &AtomicBool,
        ) -> Result<ServiceProviderResponse, ServiceProviderError> {
            Ok(ServiceProviderResponse::new(
                &self.0,
                request.generation,
                request.input.clone(),
            ))
        }
    }
}

use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, Mutex, atomic::AtomicBool},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const SERVICE_PROVIDER_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct ServiceProviderId(String);

impl ServiceProviderId {
    pub fn parse(value: &str) -> Result<Self, ServiceProviderError> {
        let value = value.trim();
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(ServiceProviderError::InvalidIdentity {
                value: value.to_owned(),
            });
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ServiceProviderId {
    type Error = ServiceProviderError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<ServiceProviderId> for String {
    fn from(value: ServiceProviderId) -> Self {
        value.0
    }
}

impl fmt::Display for ServiceProviderId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceCapabilityClass {
    RepositorySearch,
    CodeIntelligence,
    WebSearch,
    ArtifactStore,
    TerminalAdapter,
    ExternalProtocol,
    Retrieval,
    Diagnostics,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceAuthority {
    ReadOnly,
    CertifiedToolPipeline,
    FixedRuntimeAuthority,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceBoundary {
    RuntimeOwnedFilesystem,
    RuntimeOwnedNetwork,
    RuntimeOwnedProcess,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceConcurrency {
    Concurrent,
    Serialized,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceCancellation {
    Cooperative,
    NotSupported,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServiceProviderDescriptor {
    pub schema_version: u16,
    pub service_id: ServiceProviderId,
    pub provider_id: ServiceProviderId,
    pub version: String,
    pub capability: ServiceCapabilityClass,
    pub config_fingerprint: String,
    pub input_schema: String,
    pub output_schema: String,
    pub required_authority: ServiceAuthority,
    pub boundaries: Vec<ServiceBoundary>,
    pub concurrency: ServiceConcurrency,
    pub cancellation: ServiceCancellation,
}

impl ServiceProviderDescriptor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        service_id: &str,
        provider_id: &str,
        version: &str,
        capability: ServiceCapabilityClass,
        config_fingerprint: &str,
        input_schema: &str,
        output_schema: &str,
        required_authority: ServiceAuthority,
        boundaries: Vec<ServiceBoundary>,
        concurrency: ServiceConcurrency,
        cancellation: ServiceCancellation,
    ) -> Result<Self, ServiceProviderError> {
        let service_id = ServiceProviderId::parse(service_id)?;
        let provider_id = ServiceProviderId::parse(provider_id)?;
        Ok(Self {
            schema_version: SERVICE_PROVIDER_SCHEMA_VERSION,
            service_id,
            provider_id,
            version: version.to_owned(),
            capability,
            config_fingerprint: config_fingerprint.to_owned(),
            input_schema: input_schema.to_owned(),
            output_schema: output_schema.to_owned(),
            required_authority,
            boundaries,
            concurrency,
            cancellation,
        })
    }

    fn validate(&self) -> Result<(), ServiceProviderError> {
        if self.schema_version != SERVICE_PROVIDER_SCHEMA_VERSION
            || self.version.trim().is_empty()
            || self.config_fingerprint.trim().is_empty()
            || self.input_schema.trim().is_empty()
            || self.output_schema.trim().is_empty()
        {
            return Err(ServiceProviderError::InvalidDescriptor {
                provider_id: self.provider_id.to_string(),
            });
        }
        if self.required_authority == ServiceAuthority::FixedRuntimeAuthority {
            return Err(ServiceProviderError::AuthorityNotExtensible);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum ServiceReadiness {
    Ready,
    Unavailable { reason: String },
    Degraded { reason: String },
    Stopped,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServiceProviderHealth {
    pub readiness: ServiceReadiness,
    pub diagnostic: Option<String>,
}

impl ServiceProviderHealth {
    #[must_use]
    pub fn ready() -> Self {
        Self {
            readiness: ServiceReadiness::Ready,
            diagnostic: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServiceProviderRequest {
    pub schema_version: u16,
    pub service_id: ServiceProviderId,
    pub provider_id: ServiceProviderId,
    pub generation: u64,
    pub input: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServiceProviderResponse {
    pub schema_version: u16,
    pub service_id: ServiceProviderId,
    pub provider_id: ServiceProviderId,
    pub generation: u64,
    pub output: Value,
    pub evidence_fingerprint: String,
}

impl ServiceProviderResponse {
    #[must_use]
    pub fn new(descriptor: &ServiceProviderDescriptor, generation: u64, output: Value) -> Self {
        let material = serde_json::to_vec(&(
            descriptor.service_id.as_str(),
            descriptor.provider_id.as_str(),
            descriptor.version.as_str(),
            descriptor.config_fingerprint.as_str(),
            generation,
            &output,
        ))
        .expect("service provider response material is serializable");
        let evidence_fingerprint = Sha256::digest(material)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Self {
            schema_version: SERVICE_PROVIDER_SCHEMA_VERSION,
            service_id: descriptor.service_id.clone(),
            provider_id: descriptor.provider_id.clone(),
            generation,
            output,
            evidence_fingerprint,
        }
    }
}

pub trait ServiceProvider: Send + Sync {
    fn descriptor(&self) -> ServiceProviderDescriptor;
    fn start(&self, generation: u64) -> Result<(), ServiceProviderError>;
    fn stop(&self) -> Result<(), ServiceProviderError>;
    fn health(&self) -> ServiceProviderHealth;
    fn execute(
        &self,
        request: &ServiceProviderRequest,
        cancel: &AtomicBool,
    ) -> Result<ServiceProviderResponse, ServiceProviderError>;
}

#[derive(Debug, Error)]
pub enum ServiceProviderError {
    #[error("invalid service/provider identity `{value}`")]
    InvalidIdentity { value: String },
    #[error("invalid descriptor for provider `{provider_id}`")]
    InvalidDescriptor { provider_id: String },
    #[error("fixed runtime authorities cannot be service-provider plugins")]
    AuthorityNotExtensible,
    #[error("service provider `{service_id}/{provider_id}` is already registered")]
    Duplicate {
        service_id: String,
        provider_id: String,
    },
    #[error("service provider `{service_id}/{provider_id}` is not registered")]
    NotRegistered {
        service_id: String,
        provider_id: String,
    },
    #[error("service provider `{service_id}/{provider_id}` has active leases")]
    ActiveLeases {
        service_id: String,
        provider_id: String,
    },
    #[error("service provider request is bound to generation {expected}, not {actual}")]
    StaleGeneration { expected: u64, actual: u64 },
    #[error("service provider request was cancelled")]
    Cancelled,
    #[error("service provider lifecycle failed: {0}")]
    Lifecycle(String),
    #[error("service provider is not ready: {0}")]
    NotReady(String),
}

struct RegisteredProvider {
    provider: Arc<dyn ServiceProvider>,
    active_leases: usize,
}

struct RegistryState {
    generation: u64,
    providers: BTreeMap<(ServiceProviderId, ServiceProviderId), RegisteredProvider>,
}

pub struct ServiceProviderRegistry {
    state: Arc<Mutex<RegistryState>>,
}

impl ServiceProviderRegistry {
    #[must_use]
    pub fn new(generation: u64) -> Self {
        Self {
            state: Arc::new(Mutex::new(RegistryState {
                generation,
                providers: BTreeMap::new(),
            })),
        }
    }

    pub fn register(
        &mut self,
        provider: Arc<dyn ServiceProvider>,
    ) -> Result<(), ServiceProviderError> {
        let descriptor = provider.descriptor();
        descriptor.validate()?;
        let key = (
            descriptor.service_id.clone(),
            descriptor.provider_id.clone(),
        );
        let mut state = self
            .state
            .lock()
            .map_err(|_| ServiceProviderError::Lifecycle("registry lock poisoned".to_owned()))?;
        if state.providers.contains_key(&key) {
            return Err(ServiceProviderError::Duplicate {
                service_id: key.0.to_string(),
                provider_id: key.1.to_string(),
            });
        }
        state.providers.insert(
            key,
            RegisteredProvider {
                provider,
                active_leases: 0,
            },
        );
        Ok(())
    }

    pub fn admit(
        &self,
        service_id: &str,
        provider_id: &str,
    ) -> Result<ServiceProviderLease, ServiceProviderError> {
        let service_id = ServiceProviderId::parse(service_id)?;
        let provider_id = ServiceProviderId::parse(provider_id)?;
        let (provider, generation) = {
            let mut state = self.state.lock().map_err(|_| {
                ServiceProviderError::Lifecycle("registry lock poisoned".to_owned())
            })?;
            let key = (service_id.clone(), provider_id.clone());
            let generation = state.generation;
            let entry = state.providers.get_mut(&key).ok_or_else(|| {
                ServiceProviderError::NotRegistered {
                    service_id: service_id.to_string(),
                    provider_id: provider_id.to_string(),
                }
            })?;
            let health = entry.provider.health();
            if !matches!(health.readiness, ServiceReadiness::Ready) {
                return Err(ServiceProviderError::NotReady(
                    health
                        .diagnostic
                        .unwrap_or_else(|| "provider is not ready".to_owned()),
                ));
            }
            entry.provider.start(generation)?;
            entry.active_leases = entry.active_leases.saturating_add(1);
            (Arc::clone(&entry.provider), generation)
        };
        Ok(ServiceProviderLease {
            registry: Arc::clone(&self.state),
            provider,
            service_id,
            provider_id,
            generation,
            closed: false,
        })
    }

    pub fn unregister(
        &mut self,
        service_id: &str,
        provider_id: &str,
    ) -> Result<(), ServiceProviderError> {
        let service_id = ServiceProviderId::parse(service_id)?;
        let provider_id = ServiceProviderId::parse(provider_id)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| ServiceProviderError::Lifecycle("registry lock poisoned".to_owned()))?;
        let key = (service_id.clone(), provider_id.clone());
        let entry =
            state
                .providers
                .get(&key)
                .ok_or_else(|| ServiceProviderError::NotRegistered {
                    service_id: service_id.to_string(),
                    provider_id: provider_id.to_string(),
                })?;
        if entry.active_leases != 0 {
            return Err(ServiceProviderError::ActiveLeases {
                service_id: service_id.to_string(),
                provider_id: provider_id.to_string(),
            });
        }
        state.providers.remove(&key);
        Ok(())
    }

    pub fn set_generation(&mut self, generation: u64) -> Result<(), ServiceProviderError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ServiceProviderError::Lifecycle("registry lock poisoned".to_owned()))?;
        if state
            .providers
            .values()
            .any(|entry| entry.active_leases != 0)
        {
            return Err(ServiceProviderError::ActiveLeases {
                service_id: "*".to_owned(),
                provider_id: "*".to_owned(),
            });
        }
        state.generation = generation;
        Ok(())
    }

    pub fn contains_provider(&self, provider_id: &str) -> Result<bool, ServiceProviderError> {
        let provider_id = ServiceProviderId::parse(provider_id)?;
        let state = self
            .state
            .lock()
            .map_err(|_| ServiceProviderError::Lifecycle("registry lock poisoned".to_owned()))?;
        let matches = state
            .providers
            .keys()
            .filter(|(_, registered_provider_id)| registered_provider_id == &provider_id)
            .count();
        Ok(matches == 1)
    }
}

pub struct ServiceProviderLease {
    registry: Arc<Mutex<RegistryState>>,
    provider: Arc<dyn ServiceProvider>,
    service_id: ServiceProviderId,
    provider_id: ServiceProviderId,
    generation: u64,
    closed: bool,
}

impl ServiceProviderLease {
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn execute(
        &self,
        input: Value,
        cancel: &AtomicBool,
    ) -> Result<ServiceProviderResponse, ServiceProviderError> {
        self.execute_with_generation(self.generation, input, cancel)
    }

    pub fn execute_with_generation(
        &self,
        generation: u64,
        input: Value,
        cancel: &AtomicBool,
    ) -> Result<ServiceProviderResponse, ServiceProviderError> {
        if generation != self.generation {
            return Err(ServiceProviderError::StaleGeneration {
                expected: self.generation,
                actual: generation,
            });
        }
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err(ServiceProviderError::Cancelled);
        }
        let descriptor = self.provider.descriptor();
        let request = ServiceProviderRequest {
            schema_version: SERVICE_PROVIDER_SCHEMA_VERSION,
            service_id: self.service_id.clone(),
            provider_id: self.provider_id.clone(),
            generation,
            input,
        };
        self.provider
            .execute(&request, cancel)
            .map(|response| {
                if response.schema_version != SERVICE_PROVIDER_SCHEMA_VERSION
                    || response.service_id != descriptor.service_id
                    || response.provider_id != descriptor.provider_id
                    || response.generation != generation
                {
                    return Err(ServiceProviderError::InvalidDescriptor {
                        provider_id: descriptor.provider_id.to_string(),
                    });
                }
                Ok(response)
            })
            .and_then(std::convert::identity)
    }

    pub fn close(mut self) -> Result<(), ServiceProviderError> {
        self.close_inner()
    }

    fn close_inner(&mut self) -> Result<(), ServiceProviderError> {
        if self.closed {
            return Ok(());
        }
        let stop_result = self.provider.stop();
        let state_error = match self.registry.lock() {
            Ok(mut state) => {
                if let Some(entry) = state
                    .providers
                    .get_mut(&(self.service_id.clone(), self.provider_id.clone()))
                {
                    entry.active_leases = entry.active_leases.saturating_sub(1);
                }
                None
            }
            Err(_) => Some(ServiceProviderError::Lifecycle(
                "registry lock poisoned".to_owned(),
            )),
        };
        self.closed = true;
        stop_result?;
        state_error.map_or(Ok(()), Err)
    }
}

impl Drop for ServiceProviderLease {
    fn drop(&mut self) {
        let _ = self.close_inner();
    }
}
