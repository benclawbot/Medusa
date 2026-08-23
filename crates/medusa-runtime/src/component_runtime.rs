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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ComponentSpec {
    pub id: ComponentId,
    pub version: String,
    pub enabled: bool,
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
            capabilities: BTreeSet::new(),
            configuration: serde_json::Value::Null,
        }
    }

    #[must_use]
    pub fn with_capability(mut self, capability: HostCapability) -> Self {
        self.capabilities.insert(capability);
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
        Ok(())
    }
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
