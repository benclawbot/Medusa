//! Transactional component-runtime primitives.
//!
//! This module is deliberately independent from the interactive session worker.  A component is
//! a long-lived unit of ownership with a stable logical identity and a monotonically increasing
//! generation.  Later runtime layers (effects, dependency reconciliation, and desired state) use
//! the scoped context defined here instead of handing component code a global runtime handle.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ComponentId(String);

impl ComponentId {
    pub fn new(value: impl Into<String>) -> Result<Self, ComponentRuntimeError> {
        let value = value.into();
        if value.trim().is_empty()
            || value.chars().any(|character| {
                !(character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
            })
        {
            return Err(ComponentRuntimeError::InvalidComponentId(value));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ComponentGeneration(u64);

impl ComponentGeneration {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ComponentInstanceId {
    pub component_id: ComponentId,
    pub generation: ComponentGeneration,
}

impl ComponentInstanceId {
    #[must_use]
    pub fn component_id(&self) -> &ComponentId {
        &self.component_id
    }

    #[must_use]
    pub const fn generation(&self) -> ComponentGeneration {
        self.generation
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    Inactive,
    Activating,
    Active,
    Deactivating,
    Failed,
    Retiring,
    BlockedRetirement,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostCapability {
    FilesystemRead,
    FilesystemWrite,
    EnvironmentRead,
    Network,
    ProcessSpawn,
    ProcessTree,
    ResourceLimits,
    GitRead,
    GitWrite,
    CredentialUse,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    ToolRegistration,
    ServiceRegistration,
    Callback,
    ContainedProcess,
    Route,
    TemporaryRoute,
    CapabilityLease,
    FilesystemState,
    Subscription,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OwnedResource {
    pub id: String,
    pub owner: ComponentInstanceId,
    pub kind: ResourceKind,
    pub owner_process_alive: bool,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Default)]
pub struct ResourceOwnershipRegistry {
    resources: BTreeMap<String, OwnedResource>,
}

impl ResourceOwnershipRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        context: &ScopedComponentContext,
        id: impl Into<String>,
        kind: ResourceKind,
    ) -> Result<OwnedResource, ComponentRuntimeError> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(ComponentRuntimeError::InvalidResourceId(id));
        }
        if let Some(existing) = self.resources.get(&id) {
            if existing.owner != context.identity {
                return Err(ComponentRuntimeError::ResourceOwnershipConflict {
                    resource: id,
                    existing_owner: existing.owner.clone(),
                    requested_owner: context.identity.clone(),
                });
            }
            return Ok(existing.clone());
        }
        let resource = OwnedResource {
            id: id.clone(),
            owner: context.identity.clone(),
            kind,
            owner_process_alive: true,
            metadata: BTreeMap::new(),
        };
        self.resources.insert(id, resource.clone());
        Ok(resource)
    }

    pub fn release(
        &mut self,
        context: &ScopedComponentContext,
        id: &str,
    ) -> Result<OwnedResource, ComponentRuntimeError> {
        let Some(existing) = self.resources.get(id) else {
            return Err(ComponentRuntimeError::UnknownResource(id.to_owned()));
        };
        if existing.owner != context.identity {
            return Err(ComponentRuntimeError::ResourceOwnershipDenied {
                resource: id.to_owned(),
                actual_owner: existing.owner.clone(),
                requested_owner: context.identity.clone(),
            });
        }
        self.resources
            .remove(id)
            .ok_or_else(|| ComponentRuntimeError::UnknownResource(id.to_owned()))
    }

    #[must_use]
    pub fn resources_for(&self, owner: ComponentInstanceId) -> Vec<OwnedResource> {
        self.resources
            .values()
            .filter(|resource| resource.owner == owner)
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn resource(&self, id: &str) -> Option<&OwnedResource> {
        self.resources.get(id)
    }

    pub fn mark_process_dead(&mut self, owner: ComponentInstanceId) {
        for resource in self.resources.values_mut() {
            if resource.owner == owner {
                resource.owner_process_alive = false;
            }
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.resources.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }
}

pub type EffectInverse = Box<dyn FnMut() -> Result<(), String> + Send + 'static>;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectState {
    Active,
    Reverted,
    CleanupDebt,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CleanupDebt {
    pub effect_id: u64,
    pub owner: ComponentInstanceId,
    pub label: String,
    pub reason: String,
}

struct EffectEntry {
    id: u64,
    owner: ComponentInstanceId,
    label: String,
    state: EffectState,
    inverse: Option<EffectInverse>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RollbackReport {
    pub attempted: usize,
    pub reverted: usize,
    pub failures: usize,
    pub remaining: usize,
    pub cleanup_debt: Vec<CleanupDebt>,
}

impl RollbackReport {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.failures == 0 && self.remaining == 0
    }
}

pub struct EffectJournal {
    owner: ComponentInstanceId,
    next_effect_id: u64,
    effects: Vec<EffectEntry>,
    cleanup_debt: Vec<CleanupDebt>,
}

impl EffectJournal {
    #[must_use]
    pub fn new(owner: ComponentInstanceId) -> Self {
        Self {
            owner,
            next_effect_id: 1,
            effects: Vec::new(),
            cleanup_debt: Vec::new(),
        }
    }

    #[must_use]
    pub fn owner(&self) -> &ComponentInstanceId {
        &self.owner
    }

    pub fn record_successful_effect<F>(&mut self, label: impl Into<String>, inverse: F) -> u64
    where
        F: FnMut() -> Result<(), String> + Send + 'static,
    {
        let id = self.next_effect_id;
        self.next_effect_id = self.next_effect_id.saturating_add(1);
        self.effects.push(EffectEntry {
            id,
            owner: self.owner.clone(),
            label: label.into(),
            state: EffectState::Active,
            inverse: Some(Box::new(inverse)),
        });
        id
    }

    pub fn apply<T, F, I>(
        &mut self,
        label: impl Into<String>,
        forward: F,
        inverse: I,
    ) -> Result<T, String>
    where
        F: FnOnce() -> Result<T, String>,
        I: FnMut() -> Result<(), String> + Send + 'static,
    {
        let value = forward()?;
        self.record_successful_effect(label, inverse);
        Ok(value)
    }

    pub fn rollback(&mut self) -> RollbackReport {
        let mut report = RollbackReport::default();
        for index in (0..self.effects.len()).rev() {
            let (effect_id, owner, label, inverse) = {
                let entry = &mut self.effects[index];
                if entry.state == EffectState::Reverted {
                    continue;
                }
                (
                    entry.id,
                    entry.owner.clone(),
                    entry.label.clone(),
                    entry.inverse.take(),
                )
            };
            let Some(mut inverse) = inverse else {
                continue;
            };
            report.attempted += 1;
            match inverse() {
                Ok(()) => {
                    self.effects[index].state = EffectState::Reverted;
                    report.reverted += 1;
                    self.cleanup_debt.retain(|debt| debt.effect_id != effect_id);
                }
                Err(reason) => {
                    self.effects[index].state = EffectState::CleanupDebt;
                    self.effects[index].inverse = Some(inverse);
                    report.failures += 1;
                    if !self
                        .cleanup_debt
                        .iter()
                        .any(|debt| debt.effect_id == effect_id)
                    {
                        self.cleanup_debt.push(CleanupDebt {
                            effect_id,
                            owner,
                            label,
                            reason,
                        });
                    }
                }
            }
        }
        report.remaining = self.pending_effect_count();
        report.cleanup_debt = self.cleanup_debt.clone();
        report
    }

    #[must_use]
    pub fn pending_effect_count(&self) -> usize {
        self.effects
            .iter()
            .filter(|effect| effect.state != EffectState::Reverted)
            .count()
    }

    #[must_use]
    pub fn cleanup_debt(&self) -> &[CleanupDebt] {
        &self.cleanup_debt
    }

    #[must_use]
    pub fn effect_states(&self) -> Vec<(u64, EffectState)> {
        self.effects
            .iter()
            .map(|effect| (effect.id, effect.state))
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ComponentProvenance {
    pub desired_revision: u64,
    pub source: String,
}

impl ComponentProvenance {
    #[must_use]
    pub fn new(desired_revision: u64, source: impl Into<String>) -> Self {
        Self {
            desired_revision,
            source: source.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionConstraint {
    Any,
    Exact(String),
    AtLeast(String),
}

impl Default for VersionConstraint {
    fn default() -> Self {
        Self::Any
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyCardinality {
    #[default]
    One,
    Many,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DependencyRequirement {
    pub service: String,
    #[serde(default)]
    pub version: VersionConstraint,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub cardinality: DependencyCardinality,
}

impl DependencyRequirement {
    #[must_use]
    pub fn required(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            version: VersionConstraint::Any,
            optional: false,
            cardinality: DependencyCardinality::One,
        }
    }

    #[must_use]
    pub fn optional(service: impl Into<String>) -> Self {
        Self {
            optional: true,
            ..Self::required(service)
        }
    }

    #[must_use]
    pub fn with_version(mut self, version: VersionConstraint) -> Self {
        self.version = version;
        self
    }

    #[must_use]
    pub fn with_cardinality(mut self, cardinality: DependencyCardinality) -> Self {
        self.cardinality = cardinality;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProvidedService {
    pub service: String,
    pub version: String,
}

impl ProvidedService {
    #[must_use]
    pub fn new(service: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            version: version.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ComponentSpec {
    pub id: ComponentId,
    pub version: String,
    pub enabled: bool,
    #[serde(default)]
    pub requires: Vec<DependencyRequirement>,
    #[serde(default)]
    pub provides: Vec<ProvidedService>,
    pub capabilities: BTreeSet<HostCapability>,
    #[serde(default)]
    pub configuration: serde_json::Value,
}

impl ComponentSpec {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        let raw_id = id.into();
        let id = ComponentId::new(raw_id.clone()).unwrap_or_else(|_| ComponentId(raw_id));
        Self {
            id,
            version: "0.0.0".to_owned(),
            enabled: true,
            requires: Vec::new(),
            provides: Vec::new(),
            capabilities: BTreeSet::new(),
            configuration: serde_json::Value::Null,
        }
    }

    #[must_use]
    pub fn with_capability(mut self, capability: HostCapability) -> Self {
        self.capabilities.insert(capability);
        self
    }

    #[must_use]
    pub fn with_requirement(mut self, requirement: DependencyRequirement) -> Self {
        self.requires.push(requirement);
        self
    }

    #[must_use]
    pub fn with_provided_service(
        mut self,
        service: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        self.provides.push(ProvidedService::new(service, version));
        self
    }

    pub fn validate(&self) -> Result<(), ComponentRuntimeError> {
        ComponentId::new(self.id.as_str().to_owned())?;
        if self.version.trim().is_empty() {
            return Err(ComponentRuntimeError::InvalidSpec {
                component: self.id.clone(),
                reason: "version must not be empty".to_owned(),
            });
        }
        if !valid_version(&self.version) {
            return Err(ComponentRuntimeError::InvalidSpec {
                component: self.id.clone(),
                reason: format!("invalid component version: {}", self.version),
            });
        }
        let mut provided_services = BTreeSet::new();
        for provided in &self.provides {
            validate_service_name(&provided.service)?;
            if !valid_version(&provided.version) {
                return Err(ComponentRuntimeError::InvalidSpec {
                    component: self.id.clone(),
                    reason: format!("invalid provided service version: {}", provided.version),
                });
            }
            if !provided_services.insert(provided.service.clone()) {
                return Err(ComponentRuntimeError::InvalidSpec {
                    component: self.id.clone(),
                    reason: format!("service is provided more than once: {}", provided.service),
                });
            }
        }
        for requirement in &self.requires {
            validate_service_name(&requirement.service)?;
            if let VersionConstraint::Exact(version) | VersionConstraint::AtLeast(version) =
                &requirement.version
                && !valid_version(version)
            {
                return Err(ComponentRuntimeError::InvalidSpec {
                    component: self.id.clone(),
                    reason: format!("invalid required service version: {version}"),
                });
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderCandidate {
    pub identity: ComponentInstanceId,
    pub spec: ComponentSpec,
    pub retiring: bool,
}

impl ProviderCandidate {
    #[must_use]
    pub fn new(identity: ComponentInstanceId, spec: ComponentSpec) -> Self {
        Self {
            identity,
            spec,
            retiring: false,
        }
    }

    #[must_use]
    pub fn retiring(mut self, retiring: bool) -> Self {
        self.retiring = retiring;
        self
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DependencyView {
    providers: BTreeMap<String, Vec<ComponentInstanceId>>,
}

impl DependencyView {
    #[must_use]
    pub fn providers(&self, service: &str) -> &[ComponentInstanceId] {
        self.providers
            .get(service)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    #[must_use]
    pub fn provider(&self, service: &str) -> Option<&ComponentInstanceId> {
        self.providers(service).first()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.providers.values().all(Vec::is_empty)
    }

    #[must_use]
    pub fn as_map(&self) -> &BTreeMap<String, Vec<ComponentInstanceId>> {
        &self.providers
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum DependencyResolutionError {
    #[error("component {component:?} requires missing service {service:?}")]
    MissingRequired {
        component: ComponentId,
        service: String,
    },
    #[error(
        "component {component:?} has incompatible providers for {service:?}; required {required:?}, available {available:?}"
    )]
    IncompatibleVersion {
        component: ComponentId,
        service: String,
        required: VersionConstraint,
        available: Vec<String>,
    },
    #[error("component {component:?} has ambiguous providers for {service:?}: {providers:?}")]
    Ambiguous {
        component: ComponentId,
        service: String,
        providers: Vec<String>,
    },
    #[error("dependency cycle detected: {path:?}")]
    Cycle { path: Vec<String> },
    #[error("invalid service name {service:?}")]
    InvalidServiceName { service: String },
    #[error("component {component:?} has an undeclared dependency on {service:?}")]
    UndeclaredDependency {
        component: ComponentId,
        service: String,
    },
    #[error("invalid dependency specification for {component:?}: {reason}")]
    InvalidSpecification {
        component: ComponentId,
        reason: String,
    },
}

pub struct DependencyResolver;

impl DependencyResolver {
    pub fn resolve(
        consumer: &ComponentSpec,
        candidates: &[ProviderCandidate],
    ) -> Result<DependencyView, DependencyResolutionError> {
        consumer
            .validate()
            .map_err(|error| DependencyResolutionError::InvalidSpecification {
                component: consumer.id.clone(),
                reason: error.to_string(),
            })?;
        let mut view = DependencyView::default();
        for requirement in &consumer.requires {
            let service_candidates = candidates
                .iter()
                .filter(|candidate| {
                    candidate.spec.enabled
                        && !candidate.retiring
                        && candidate
                            .spec
                            .provides
                            .iter()
                            .any(|provided| provided.service == requirement.service)
                })
                .collect::<Vec<_>>();
            if service_candidates.is_empty() {
                if requirement.optional {
                    continue;
                }
                return Err(DependencyResolutionError::MissingRequired {
                    component: consumer.id.clone(),
                    service: requirement.service.clone(),
                });
            }
            let matching = service_candidates
                .iter()
                .filter(|candidate| {
                    candidate.spec.provides.iter().any(|provided| {
                        provided.service == requirement.service
                            && requirement.version.matches(&provided.version)
                    })
                })
                .collect::<Vec<_>>();
            if matching.is_empty() {
                return Err(DependencyResolutionError::IncompatibleVersion {
                    component: consumer.id.clone(),
                    service: requirement.service.clone(),
                    required: requirement.version.clone(),
                    available: service_candidates
                        .iter()
                        .flat_map(|candidate| {
                            candidate
                                .spec
                                .provides
                                .iter()
                                .filter(|provided| provided.service == requirement.service)
                                .map(|provided| provided.version.clone())
                        })
                        .collect(),
                });
            }
            if requirement.cardinality == DependencyCardinality::One && matching.len() > 1 {
                let mut providers = matching
                    .iter()
                    .map(|candidate| candidate.identity.component_id.as_str().to_owned())
                    .collect::<Vec<_>>();
                providers.sort();
                return Err(DependencyResolutionError::Ambiguous {
                    component: consumer.id.clone(),
                    service: requirement.service.clone(),
                    providers,
                });
            }
            let mut identities = matching
                .iter()
                .map(|candidate| candidate.identity.clone())
                .collect::<Vec<_>>();
            identities.sort();
            view.providers
                .insert(requirement.service.clone(), identities);
        }
        Ok(view)
    }

    pub fn validate_graph(specs: &[ComponentSpec]) -> Result<(), DependencyResolutionError> {
        let candidates = specs
            .iter()
            .cloned()
            .map(|spec| {
                let identity = ComponentInstanceId {
                    component_id: spec.id.clone(),
                    generation: ComponentGeneration::new(1),
                };
                ProviderCandidate::new(identity, spec)
            })
            .collect::<Vec<_>>();
        let mut edges = BTreeMap::<ComponentId, Vec<ComponentId>>::new();
        for spec in specs {
            let view = Self::resolve(spec, &candidates)?;
            for providers in view.providers.values() {
                for provider in providers {
                    if provider.component_id != spec.id {
                        edges
                            .entry(spec.id.clone())
                            .or_default()
                            .push(provider.component_id.clone());
                    }
                }
            }
        }
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        let mut path = Vec::new();
        for component in specs.iter().map(|spec| spec.id.clone()) {
            if let Some(cycle) =
                detect_cycle(&component, &edges, &mut visiting, &mut visited, &mut path)
            {
                return Err(DependencyResolutionError::Cycle { path: cycle });
            }
        }
        Ok(())
    }
}

impl VersionConstraint {
    #[must_use]
    pub fn matches(&self, version: &str) -> bool {
        match self {
            Self::Any => valid_version(version),
            Self::Exact(expected) => expected == version,
            Self::AtLeast(minimum) => {
                compare_versions(version, minimum).is_some_and(|ordering| ordering.is_ge())
            }
        }
    }
}

fn detect_cycle(
    component: &ComponentId,
    edges: &BTreeMap<ComponentId, Vec<ComponentId>>,
    visiting: &mut BTreeSet<ComponentId>,
    visited: &mut BTreeSet<ComponentId>,
    path: &mut Vec<ComponentId>,
) -> Option<Vec<String>> {
    if visiting.contains(component) {
        let start = path.iter().position(|item| item == component).unwrap_or(0);
        let mut cycle = path[start..]
            .iter()
            .map(|item| item.as_str().to_owned())
            .collect::<Vec<_>>();
        cycle.push(component.as_str().to_owned());
        return Some(cycle);
    }
    if !visited.insert(component.clone()) {
        return None;
    }
    visiting.insert(component.clone());
    path.push(component.clone());
    if let Some(dependencies) = edges.get(component) {
        for dependency in dependencies {
            if let Some(cycle) = detect_cycle(dependency, edges, visiting, visited, path) {
                return Some(cycle);
            }
        }
    }
    path.pop();
    visiting.remove(component);
    None
}

fn validate_service_name(service: &str) -> Result<(), ComponentRuntimeError> {
    if service.trim().is_empty()
        || service.chars().any(|character| {
            !(character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
        })
    {
        return Err(ComponentRuntimeError::InvalidSpec {
            component: ComponentId(service.to_owned()),
            reason: "service name must be lowercase dotted, kebab, or snake case".to_owned(),
        });
    }
    Ok(())
}

fn valid_version(version: &str) -> bool {
    !version.is_empty()
        && version.split('.').all(|part| {
            !part.is_empty() && part.chars().all(|character| character.is_ascii_digit())
        })
}

fn compare_versions(left: &str, right: &str) -> Option<std::cmp::Ordering> {
    let left = left
        .split('.')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let right = right
        .split('.')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let length = left.len().max(right.len());
    for index in 0..length {
        let ordering = left
            .get(index)
            .copied()
            .unwrap_or(0)
            .cmp(&right.get(index).copied().unwrap_or(0));
        if ordering != std::cmp::Ordering::Equal {
            return Some(ordering);
        }
    }
    Some(std::cmp::Ordering::Equal)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ScopedComponentContext {
    identity: ComponentInstanceId,
    provenance: ComponentProvenance,
    capabilities: BTreeSet<HostCapability>,
}

impl ScopedComponentContext {
    #[must_use]
    pub fn identity(&self) -> ComponentInstanceId {
        self.identity.clone()
    }

    #[must_use]
    pub fn component_id(&self) -> &ComponentId {
        &self.identity.component_id
    }

    #[must_use]
    pub const fn generation(&self) -> ComponentGeneration {
        self.identity.generation
    }

    #[must_use]
    pub const fn desired_revision(&self) -> u64 {
        self.provenance.desired_revision
    }

    #[must_use]
    pub fn provenance(&self) -> &ComponentProvenance {
        &self.provenance
    }

    #[must_use]
    pub fn capabilities(&self) -> &BTreeSet<HostCapability> {
        &self.capabilities
    }

    #[must_use]
    pub fn has_capability(&self, capability: HostCapability) -> bool {
        self.capabilities.contains(&capability)
    }

    pub fn require_capability(
        &self,
        capability: HostCapability,
    ) -> Result<(), ComponentRuntimeError> {
        if self.has_capability(capability) {
            Ok(())
        } else {
            Err(ComponentRuntimeError::CapabilityDenied {
                component: self.identity.clone(),
                capability,
            })
        }
    }
}

#[derive(Debug, Error)]
pub enum ComponentRuntimeError {
    #[error("invalid component id: {0}")]
    InvalidComponentId(String),
    #[error("invalid component specification for {component:?}: {reason}")]
    InvalidSpec {
        component: ComponentId,
        reason: String,
    },
    #[error("unknown component instance {0:?}")]
    UnknownInstance(ComponentInstanceId),
    #[error("component {component:?} is not allowed to use {capability:?}")]
    CapabilityDenied {
        component: ComponentInstanceId,
        capability: HostCapability,
    },
    #[error("resource id must not be empty")]
    InvalidResourceId(String),
    #[error(
        "resource {resource:?} is already owned by {existing_owner:?}; cannot assign it to {requested_owner:?}"
    )]
    ResourceOwnershipConflict {
        resource: String,
        existing_owner: ComponentInstanceId,
        requested_owner: ComponentInstanceId,
    },
    #[error(
        "component {requested_owner:?} does not own resource {resource:?}; it belongs to {actual_owner:?}"
    )]
    ResourceOwnershipDenied {
        resource: String,
        actual_owner: ComponentInstanceId,
        requested_owner: ComponentInstanceId,
    },
    #[error("unknown resource {0:?}")]
    UnknownResource(String),
}

struct ComponentRecord {
    spec: ComponentSpec,
    context: ScopedComponentContext,
    state: LifecycleState,
}

#[derive(Default)]
pub struct ComponentRuntime {
    next_generations: BTreeMap<ComponentId, ComponentGeneration>,
    instances: BTreeMap<ComponentInstanceId, ComponentRecord>,
}

impl ComponentRuntime {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn instantiate(
        &mut self,
        spec: ComponentSpec,
        desired_revision: u64,
    ) -> Result<ComponentInstanceId, ComponentRuntimeError> {
        spec.validate()?;
        let next = self
            .next_generations
            .get(&spec.id)
            .copied()
            .map_or(ComponentGeneration::new(1), |generation| {
                ComponentGeneration::new(generation.get().saturating_add(1))
            });
        self.next_generations.insert(spec.id.clone(), next);
        let identity = ComponentInstanceId {
            component_id: spec.id.clone(),
            generation: next,
        };
        let context = ScopedComponentContext {
            identity: identity.clone(),
            provenance: ComponentProvenance::new(desired_revision, "component-runtime"),
            capabilities: spec.capabilities.clone(),
        };
        self.instances.insert(
            identity.clone(),
            ComponentRecord {
                spec,
                context,
                state: LifecycleState::Inactive,
            },
        );
        Ok(identity)
    }

    #[must_use]
    pub fn lifecycle_state(&self, identity: &ComponentInstanceId) -> Option<LifecycleState> {
        self.instances.get(identity).map(|record| record.state)
    }

    pub fn context(
        &self,
        identity: &ComponentInstanceId,
    ) -> Result<ScopedComponentContext, ComponentRuntimeError> {
        self.instances
            .get(identity)
            .map(|record| record.context.clone())
            .ok_or_else(|| ComponentRuntimeError::UnknownInstance(identity.clone()))
    }

    #[must_use]
    pub fn spec(&self, identity: &ComponentInstanceId) -> Option<&ComponentSpec> {
        self.instances.get(identity).map(|record| &record.spec)
    }
}
