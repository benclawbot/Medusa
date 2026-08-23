//! Transactional component-runtime primitives.
//!
//! This module is deliberately independent from the interactive session worker.  A component is
//! a long-lived unit of ownership with a stable logical identity and a monotonically increasing
//! generation.  Later runtime layers (effects, dependency reconciliation, and desired state) use
//! the scoped context defined here instead of handing component code a global runtime handle.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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
pub enum ContainmentControl {
    Filesystem,
    Environment,
    Network,
    Process,
    ResourceLimits,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContainmentPlatform {
    pub name: String,
    supported: BTreeSet<ContainmentControl>,
}

impl ContainmentPlatform {
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        supported: impl IntoIterator<Item = ContainmentControl>,
    ) -> Self {
        Self {
            name: name.into(),
            supported: supported.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn current() -> Self {
        Self::new(
            std::env::consts::OS,
            [
                ContainmentControl::Filesystem,
                ContainmentControl::Environment,
                ContainmentControl::Network,
                ContainmentControl::Process,
                ContainmentControl::ResourceLimits,
            ],
        )
    }

    #[must_use]
    pub fn supports(&self, control: ContainmentControl) -> bool {
        self.supported.contains(&control)
    }

    #[must_use]
    pub fn supported_controls(&self) -> &BTreeSet<ContainmentControl> {
        &self.supported
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostAuthority {
    allowed: BTreeSet<HostCapability>,
}

impl HostAuthority {
    #[must_use]
    pub fn has(&self, capability: HostCapability) -> bool {
        self.allowed.contains(&capability)
    }

    pub fn require(&self, capability: HostCapability) -> Result<(), ContainmentPolicyError> {
        if self.has(capability) {
            Ok(())
        } else {
            Err(ContainmentPolicyError::CapabilityDenied { capability })
        }
    }

    #[must_use]
    pub fn capabilities(&self) -> &BTreeSet<HostCapability> {
        &self.allowed
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResolvedCapabilityPolicy {
    pub component: ComponentInstanceId,
    pub desired_revision: u64,
    pub declared: BTreeSet<HostCapability>,
    pub host_authority: HostAuthority,
    pub os_controls: BTreeSet<ContainmentControl>,
    pub unsupported: Vec<ContainmentControl>,
    pub policy_generation: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum ContainmentPolicyError {
    #[error("capability {capability:?} is not declared for the component")]
    CapabilityDenied { capability: HostCapability },
    #[error(
        "containment control {control:?} requested by {capability:?} is unsupported on {platform}"
    )]
    Unsupported {
        platform: String,
        control: ContainmentControl,
        capability: HostCapability,
    },
    #[error("capability policy is invalid: {0}")]
    Invalid(String),
}

pub struct CapabilityPolicyCompiler;

impl CapabilityPolicyCompiler {
    pub fn compile(
        spec: &ComponentSpec,
        identity: &ComponentInstanceId,
        desired_revision: u64,
        platform: &ContainmentPlatform,
    ) -> Result<ResolvedCapabilityPolicy, ContainmentPolicyError> {
        spec.validate()
            .map_err(|error| ContainmentPolicyError::Invalid(error.to_string()))?;
        if &spec.id != identity.component_id() {
            return Err(ContainmentPolicyError::Invalid(
                "component identity does not match component specification".to_owned(),
            ));
        }
        let declared = spec.capabilities.clone();
        let host_authority = HostAuthority {
            allowed: declared.clone(),
        };
        let mut os_controls = BTreeSet::new();
        for capability in &declared {
            let Some(control) = containment_control(*capability) else {
                continue;
            };
            if !platform.supports(control) {
                return Err(ContainmentPolicyError::Unsupported {
                    platform: platform.name.clone(),
                    control,
                    capability: *capability,
                });
            }
            os_controls.insert(control);
        }
        let policy_generation = policy_fingerprint(identity, desired_revision, &declared);
        Ok(ResolvedCapabilityPolicy {
            component: identity.clone(),
            desired_revision,
            declared,
            host_authority,
            os_controls,
            unsupported: Vec::new(),
            policy_generation,
        })
    }

    pub fn validate_spec(spec: &ComponentSpec) -> Result<(), ContainmentPolicyError> {
        let identity = ComponentInstanceId {
            component_id: spec.id.clone(),
            generation: ComponentGeneration::new(0),
        };
        Self::compile(spec, &identity, 0, &ContainmentPlatform::current()).map(|_| ())
    }
}

fn containment_control(capability: HostCapability) -> Option<ContainmentControl> {
    match capability {
        HostCapability::FilesystemRead
        | HostCapability::FilesystemWrite
        | HostCapability::GitRead
        | HostCapability::GitWrite => Some(ContainmentControl::Filesystem),
        HostCapability::EnvironmentRead => Some(ContainmentControl::Environment),
        HostCapability::Network => Some(ContainmentControl::Network),
        HostCapability::ProcessSpawn | HostCapability::ProcessTree => {
            Some(ContainmentControl::Process)
        }
        HostCapability::ResourceLimits => Some(ContainmentControl::ResourceLimits),
        HostCapability::CredentialUse => None,
    }
}

fn policy_fingerprint(
    identity: &ComponentInstanceId,
    desired_revision: u64,
    declared: &BTreeSet<HostCapability>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(identity.component_id.as_str().as_bytes());
    hasher.update(identity.generation.get().to_le_bytes());
    hasher.update(desired_revision.to_le_bytes());
    for capability in declared {
        hasher.update(format!("{capability:?};").as_bytes());
    }
    format!("sha256:{:x}", hasher.finalize())
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalCommitSemantics {
    AtMostOnce,
    AtLeastOnce,
    CompensationRequired,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExternalCommitRequest {
    pub operation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    pub semantics: ExternalCommitSemantics,
    pub payload_digest: String,
    pub source: String,
}

impl ExternalCommitRequest {
    #[must_use]
    pub fn new(
        operation_id: impl Into<String>,
        semantics: ExternalCommitSemantics,
        payload_digest: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            operation_id: operation_id.into(),
            idempotency_key: None,
            semantics,
            payload_digest: payload_digest.into(),
            source: source.into(),
        }
    }

    #[must_use]
    pub fn with_idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = Some(key.into());
        self
    }

    fn validate(&self) -> Result<(), ExternalCommitError> {
        if self.operation_id.trim().is_empty() {
            return Err(ExternalCommitError::InvalidRequest {
                reason: "operation id must not be empty".to_owned(),
            });
        }
        if self.payload_digest.trim().is_empty() {
            return Err(ExternalCommitError::InvalidRequest {
                reason: "payload digest must not be empty".to_owned(),
            });
        }
        if self.source.trim().is_empty() {
            return Err(ExternalCommitError::InvalidRequest {
                reason: "source must not be empty".to_owned(),
            });
        }
        if self
            .idempotency_key
            .as_deref()
            .is_some_and(|key| key.trim().is_empty())
        {
            return Err(ExternalCommitError::InvalidRequest {
                reason: "idempotency key must not be empty".to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalCommitStatus {
    Prepared,
    Committed,
    Unknown,
    CompensationRequired,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExternalCommitRecord {
    pub request: ExternalCommitRequest,
    pub status: ExternalCommitStatus,
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
}

#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum ExternalCommitError {
    #[error("external commit request is invalid: {reason}")]
    InvalidRequest { reason: String },
    #[error("external operation {operation_id:?} was reused with a different payload or semantics")]
    OperationConflict { operation_id: String },
    #[error("idempotency key {idempotency_key:?} was reused for a different operation")]
    IdempotencyConflict { idempotency_key: String },
    #[error("external operation {operation_id:?} is not present in the ledger")]
    UnknownOperation { operation_id: String },
    #[error("external operation {operation_id:?} cannot transition from {status:?} to {target:?}")]
    InvalidTransition {
        operation_id: String,
        status: ExternalCommitStatus,
        target: ExternalCommitStatus,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExternalCommitLedger {
    records: BTreeMap<String, ExternalCommitRecord>,
}

impl ExternalCommitLedger {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn prepare(
        &mut self,
        request: ExternalCommitRequest,
    ) -> Result<ExternalCommitRecord, ExternalCommitError> {
        request.validate()?;
        if let Some(existing) = self.records.get(&request.operation_id) {
            if existing.request == request {
                return Ok(existing.clone());
            }
            return Err(ExternalCommitError::OperationConflict {
                operation_id: request.operation_id,
            });
        }
        if let Some(idempotency_key) = request.idempotency_key.as_deref()
            && let Some(existing) = self
                .records
                .values()
                .find(|record| record.request.idempotency_key.as_deref() == Some(idempotency_key))
        {
            if existing.request.payload_digest == request.payload_digest
                && existing.request.semantics == request.semantics
            {
                return Ok(existing.clone());
            }
            return Err(ExternalCommitError::IdempotencyConflict {
                idempotency_key: idempotency_key.to_owned(),
            });
        }
        let record = ExternalCommitRecord {
            request: request.clone(),
            status: ExternalCommitStatus::Prepared,
            attempts: 1,
            failure_reason: None,
        };
        self.records.insert(request.operation_id, record.clone());
        Ok(record)
    }

    pub fn mark_committed(
        &mut self,
        operation_id: &str,
    ) -> Result<ExternalCommitRecord, ExternalCommitError> {
        self.transition(operation_id, ExternalCommitStatus::Committed, None)
    }

    pub fn mark_unknown(
        &mut self,
        operation_id: &str,
        reason: impl Into<String>,
    ) -> Result<ExternalCommitRecord, ExternalCommitError> {
        self.transition(
            operation_id,
            ExternalCommitStatus::Unknown,
            Some(reason.into()),
        )
    }

    pub fn mark_compensation_required(
        &mut self,
        operation_id: &str,
        reason: impl Into<String>,
    ) -> Result<ExternalCommitRecord, ExternalCommitError> {
        self.transition(
            operation_id,
            ExternalCommitStatus::CompensationRequired,
            Some(reason.into()),
        )
    }

    pub fn retry(
        &mut self,
        operation_id: &str,
    ) -> Result<ExternalCommitRecord, ExternalCommitError> {
        let record = self.records.get_mut(operation_id).ok_or_else(|| {
            ExternalCommitError::UnknownOperation {
                operation_id: operation_id.to_owned(),
            }
        })?;
        if record.status != ExternalCommitStatus::Unknown
            || record.request.semantics != ExternalCommitSemantics::AtLeastOnce
        {
            return Err(ExternalCommitError::InvalidTransition {
                operation_id: operation_id.to_owned(),
                status: record.status,
                target: ExternalCommitStatus::Prepared,
            });
        }
        record.status = ExternalCommitStatus::Prepared;
        record.attempts = record.attempts.saturating_add(1);
        record.failure_reason = None;
        Ok(record.clone())
    }

    fn transition(
        &mut self,
        operation_id: &str,
        target: ExternalCommitStatus,
        reason: Option<String>,
    ) -> Result<ExternalCommitRecord, ExternalCommitError> {
        let record = self.records.get_mut(operation_id).ok_or_else(|| {
            ExternalCommitError::UnknownOperation {
                operation_id: operation_id.to_owned(),
            }
        })?;
        if record.status == target {
            return Ok(record.clone());
        }
        let allowed = matches!(
            (record.status, target),
            (
                ExternalCommitStatus::Prepared,
                ExternalCommitStatus::Committed
            ) | (
                ExternalCommitStatus::Prepared,
                ExternalCommitStatus::Unknown
            ) | (
                ExternalCommitStatus::Prepared,
                ExternalCommitStatus::CompensationRequired
            ) | (
                ExternalCommitStatus::Unknown,
                ExternalCommitStatus::CompensationRequired
            )
        );
        if !allowed {
            return Err(ExternalCommitError::InvalidTransition {
                operation_id: operation_id.to_owned(),
                status: record.status,
                target,
            });
        }
        record.status = target;
        record.failure_reason = reason;
        Ok(record.clone())
    }

    #[must_use]
    pub fn record(&self, operation_id: &str) -> Option<&ExternalCommitRecord> {
        self.records.get(operation_id)
    }

    #[must_use]
    pub fn retryable(&self, operation_id: &str) -> Option<bool> {
        let record = self.records.get(operation_id)?;
        Some(match record.status {
            ExternalCommitStatus::Prepared => true,
            ExternalCommitStatus::Committed => false,
            ExternalCommitStatus::Unknown => {
                record.request.semantics == ExternalCommitSemantics::AtLeastOnce
            }
            ExternalCommitStatus::CompensationRequired => false,
        })
    }

    #[must_use]
    pub fn records(&self) -> &BTreeMap<String, ExternalCommitRecord> {
        &self.records
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultPoint {
    ActivationEffect,
    CandidateHealth,
    ConsumerTeardown,
    ProviderTeardown,
    DesiredStatePersist,
    ReconciliationCommit,
    ExternalPrepare,
    ExternalCommit,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FaultTraceEvent {
    pub sequence: u64,
    pub point: FaultPoint,
    pub injected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum InjectedFaultError {
    #[error("fault injected at {point:?}: {reason}")]
    Injected { point: FaultPoint, reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FaultInjector {
    seed: u64,
    failures: BTreeMap<FaultPoint, String>,
    trace: Vec<FaultTraceEvent>,
}

impl FaultInjector {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            failures: BTreeMap::new(),
            trace: Vec::new(),
        }
    }

    #[must_use]
    pub const fn seed(&self) -> u64 {
        self.seed
    }

    pub fn fail_once(&mut self, point: FaultPoint, reason: impl Into<String>) {
        self.failures.insert(point, reason.into());
    }

    pub fn check(&mut self, point: FaultPoint) -> Result<(), InjectedFaultError> {
        let reason = self.failures.remove(&point);
        let event = FaultTraceEvent {
            sequence: self.trace.len() as u64,
            point,
            injected: reason.is_some(),
            reason: reason.clone(),
        };
        self.trace.push(event);
        if let Some(reason) = reason {
            Err(InjectedFaultError::Injected { point, reason })
        } else {
            Ok(())
        }
    }

    #[must_use]
    pub fn trace(&self) -> &[FaultTraceEvent] {
        &self.trace
    }

    #[must_use]
    pub fn replay_fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.seed.to_le_bytes());
        for event in &self.trace {
            hasher.update(event.sequence.to_le_bytes());
            hasher.update(format!("{:?};{};", event.point, event.injected).as_bytes());
            if let Some(reason) = &event.reason {
                hasher.update(reason.as_bytes());
            }
        }
        format!("sha256:{:x}", hasher.finalize())
    }
}

impl EffectJournal {
    pub fn record_external_commit(
        &mut self,
        request: &ExternalCommitRequest,
    ) -> Result<(), ComponentRuntimeError> {
        Err(ComponentRuntimeError::ExternalCommitNotReversible {
            operation_id: request.operation_id.clone(),
        })
    }

    pub fn apply_with_fault<T, F, I>(
        &mut self,
        injector: &mut FaultInjector,
        point: FaultPoint,
        label: impl Into<String>,
        forward: F,
        inverse: I,
    ) -> Result<T, String>
    where
        F: FnOnce() -> Result<T, String>,
        I: FnMut() -> Result<(), String> + Send + 'static,
    {
        injector.check(point).map_err(|error| error.to_string())?;
        self.apply(label, forward, inverse)
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
    pub exclusive_resources: BTreeSet<String>,
    #[serde(default)]
    pub configuration: serde_json::Value,
}

impl ComponentSpec {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        let raw_id = id.into();
        let id = ComponentId::new(raw_id.clone()).unwrap_or(ComponentId(raw_id));
        Self {
            id,
            version: "0.0.0".to_owned(),
            enabled: true,
            requires: Vec::new(),
            provides: Vec::new(),
            capabilities: BTreeSet::new(),
            exclusive_resources: BTreeSet::new(),
            configuration: serde_json::Value::Null,
        }
    }

    #[must_use]
    pub fn with_capability(mut self, capability: HostCapability) -> Self {
        self.capabilities.insert(capability);
        self
    }

    #[must_use]
    pub fn with_exclusive_resource(mut self, resource: impl Into<String>) -> Self {
        self.exclusive_resources.insert(resource.into());
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
        if self
            .exclusive_resources
            .iter()
            .any(|resource| resource.trim().is_empty())
        {
            return Err(ComponentRuntimeError::InvalidSpec {
                component: self.id.clone(),
                reason: "exclusive resource names must not be empty".to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderCandidate {
    pub identity: ComponentInstanceId,
    pub spec: ComponentSpec,
    pub retiring: bool,
    pub available: bool,
}

impl ProviderCandidate {
    #[must_use]
    pub fn new(identity: ComponentInstanceId, spec: ComponentSpec) -> Self {
        Self {
            identity,
            spec,
            retiring: false,
            available: true,
        }
    }

    #[must_use]
    pub fn retiring(mut self, retiring: bool) -> Self {
        self.retiring = retiring;
        self
    }

    #[must_use]
    pub fn available(mut self, available: bool) -> Self {
        self.available = available;
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DependencyReconciliationAction {
    Noop {
        component: ComponentInstanceId,
    },
    Restart {
        component: ComponentInstanceId,
        committed: DependencyView,
        target: DependencyView,
    },
    Deactivate {
        component: ComponentInstanceId,
        reason: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderRetirementReport {
    pub provider: ComponentInstanceId,
    pub consumers: Vec<ComponentInstanceId>,
    pub order: Vec<ComponentInstanceId>,
    pub blocked: Vec<ComponentInstanceId>,
    pub withdrawn: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ReplacementOptions {
    pub cancellation: Option<Arc<AtomicBool>>,
    pub timeout: Option<Duration>,
}

impl ReplacementOptions {
    fn check(&self, started: Instant) -> Result<(), ReplacementError> {
        if self
            .cancellation
            .as_ref()
            .is_some_and(|cancellation| cancellation.load(Ordering::SeqCst))
        {
            return Err(ReplacementError::Cancelled);
        }
        if self
            .timeout
            .is_some_and(|timeout| started.elapsed() >= timeout)
        {
            return Err(ReplacementError::TimedOut);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplacementOutcome {
    pub old: ComponentInstanceId,
    pub candidate: ComponentInstanceId,
    pub migrated_consumers: Vec<ComponentInstanceId>,
    pub old_withdrawn: bool,
}

#[derive(Debug, Error)]
pub enum ReplacementError {
    #[error(transparent)]
    Runtime(ComponentRuntimeError),
    #[error("component replacement was cancelled before commit")]
    Cancelled,
    #[error("component replacement timed out before commit")]
    TimedOut,
    #[error("candidate generation was rejected: {reason}")]
    CandidateRejected {
        reason: String,
        cleanup_debt: Vec<CleanupDebt>,
    },
    #[error("candidate generation cannot replace a different logical component")]
    ComponentIdMismatch,
    #[error("old provider cleanup was blocked after candidate validation: {reason}")]
    OldProviderCleanupBlocked { reason: String },
    #[error(
        "exclusive resource {resource:?} is owned by {owner:?}; candidate {requested:?} cannot prepare"
    )]
    ExclusiveResourceConflict {
        resource: String,
        owner: ComponentInstanceId,
        requested: ComponentInstanceId,
    },
}

impl From<ComponentRuntimeError> for ReplacementError {
    fn from(error: ComponentRuntimeError) -> Self {
        match error {
            ComponentRuntimeError::ExclusiveResourceConflict {
                resource,
                owner,
                requested,
            } => Self::ExclusiveResourceConflict {
                resource,
                owner,
                requested,
            },
            other => Self::Runtime(other),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DesiredRuntimeState {
    pub revision: u64,
    pub components: BTreeMap<ComponentId, ComponentSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DesiredStateMutation {
    Upsert(ComponentSpec),
    Remove(ComponentId),
    SetEnabled {
        component: ComponentId,
        enabled: bool,
    },
    Batch(Vec<DesiredStateMutation>),
}

impl DesiredStateMutation {
    #[must_use]
    pub fn upsert(spec: ComponentSpec) -> Self {
        Self::Upsert(spec)
    }

    #[must_use]
    pub fn remove(component: ComponentId) -> Self {
        Self::Remove(component)
    }

    #[must_use]
    pub fn set_enabled(component: ComponentId, enabled: bool) -> Self {
        Self::SetEnabled { component, enabled }
    }

    fn apply(&self, state: &mut DesiredRuntimeState) -> Result<(), DesiredStateError> {
        self.apply_raw(state)?;
        let specs = state.components.values().cloned().collect::<Vec<_>>();
        DependencyResolver::validate_graph(&specs).map_err(|error| {
            DesiredStateError::Validation {
                reason: error.to_string(),
            }
        })?;
        Ok(())
    }

    fn apply_raw(&self, state: &mut DesiredRuntimeState) -> Result<(), DesiredStateError> {
        match self {
            Self::Upsert(spec) => {
                spec.validate()
                    .map_err(|error| DesiredStateError::Validation {
                        reason: error.to_string(),
                    })?;
                CapabilityPolicyCompiler::validate_spec(spec).map_err(|error| {
                    DesiredStateError::Validation {
                        reason: error.to_string(),
                    }
                })?;
                state.components.insert(spec.id.clone(), spec.clone());
            }
            Self::Remove(component) => {
                state.components.remove(component);
            }
            Self::SetEnabled { component, enabled } => {
                let spec = state.components.get_mut(component).ok_or_else(|| {
                    DesiredStateError::Validation {
                        reason: format!(
                            "cannot change enabled state of unknown component {component:?}"
                        ),
                    }
                })?;
                spec.enabled = *enabled;
            }
            Self::Batch(mutations) => {
                for mutation in mutations {
                    mutation.apply_raw(state)?;
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DesiredStateCommit {
    pub revision: u64,
    pub snapshot: DesiredRuntimeState,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Error)]
pub enum DesiredStateError {
    #[error("desired-state revision conflict: expected {expected}, current {current:?}")]
    RevisionConflict {
        expected: u64,
        current: DesiredRuntimeState,
    },
    #[error("desired-state validation failed: {reason}")]
    Validation { reason: String },
    #[error("desired-state persistence failed: {0}")]
    Persistence(String),
    #[error("desired-state serialization failed: {0}")]
    Serialization(String),
}

struct DesiredStateStoreInner {
    state: DesiredRuntimeState,
    idempotent_commits: BTreeMap<String, DesiredStateCommit>,
    audits: Vec<ProposalAuditRecord>,
}

pub struct DesiredStateStore {
    inner: Mutex<DesiredStateStoreInner>,
    path: Option<PathBuf>,
}

impl DesiredStateStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(DesiredStateStoreInner {
                state: DesiredRuntimeState::default(),
                idempotent_commits: BTreeMap::new(),
                audits: Vec::new(),
            }),
            path: None,
        }
    }

    pub fn open(path: impl Into<PathBuf>) -> Result<Self, DesiredStateError> {
        let path = path.into();
        let state = if path.is_file() {
            let bytes = fs::read(&path)
                .map_err(|error| DesiredStateError::Persistence(error.to_string()))?;
            serde_json::from_slice(&bytes)
                .map_err(|error| DesiredStateError::Serialization(error.to_string()))?
        } else {
            DesiredRuntimeState::default()
        };
        Ok(Self {
            inner: Mutex::new(DesiredStateStoreInner {
                state,
                idempotent_commits: BTreeMap::new(),
                audits: Vec::new(),
            }),
            path: Some(path),
        })
    }

    #[must_use]
    pub fn snapshot(&self) -> DesiredRuntimeState {
        self.inner.lock().expect("desired-state lock").state.clone()
    }

    pub fn compare_and_swap(
        &self,
        expected_revision: u64,
        mutation: DesiredStateMutation,
        source: impl Into<String>,
    ) -> Result<DesiredStateCommit, DesiredStateError> {
        self.compare_and_swap_with_idempotency(expected_revision, mutation, source, None)
    }

    pub fn compare_and_swap_with_idempotency(
        &self,
        expected_revision: u64,
        mutation: DesiredStateMutation,
        source: impl Into<String>,
        idempotency_key: Option<String>,
    ) -> Result<DesiredStateCommit, DesiredStateError> {
        let mut inner = self.inner.lock().expect("desired-state lock");
        if let Some(key) = idempotency_key.as_deref()
            && let Some(previous) = inner.idempotent_commits.get(key)
        {
            return Ok(previous.clone());
        }
        if inner.state.revision != expected_revision {
            return Err(DesiredStateError::RevisionConflict {
                expected: expected_revision,
                current: inner.state.clone(),
            });
        }
        let mut next = inner.state.clone();
        mutation.apply(&mut next)?;
        next.revision = next.revision.saturating_add(1);
        next.provenance = Some(source.into());
        if let Some(path) = &self.path {
            persist_desired_state(path, &next)?;
        }
        let commit = DesiredStateCommit {
            revision: next.revision,
            snapshot: next.clone(),
            idempotency_key,
        };
        inner.state = next;
        if let Some(key) = commit.idempotency_key.as_ref() {
            inner.idempotent_commits.insert(key.clone(), commit.clone());
        }
        Ok(commit)
    }

    pub fn record_audit(&self, audit: ProposalAuditRecord) {
        self.inner
            .lock()
            .expect("desired-state lock")
            .audits
            .push(audit);
    }

    #[must_use]
    pub fn audit_records(&self) -> Vec<ProposalAuditRecord> {
        self.inner
            .lock()
            .expect("desired-state lock")
            .audits
            .clone()
    }
}

impl Default for DesiredStateStore {
    fn default() -> Self {
        Self::new()
    }
}

fn persist_desired_state(
    path: &PathBuf,
    state: &DesiredRuntimeState,
) -> Result<(), DesiredStateError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| DesiredStateError::Persistence(error.to_string()))?;
    }
    let temporary = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|error| DesiredStateError::Serialization(error.to_string()))?;
    fs::write(&temporary, bytes)
        .map_err(|error| DesiredStateError::Persistence(error.to_string()))?;
    if let Err(error) = fs::rename(&temporary, path) {
        if path.exists() {
            fs::remove_file(path)
                .and_then(|_| fs::rename(&temporary, path))
                .map_err(|_| DesiredStateError::Persistence(error.to_string()))?;
        } else {
            return Err(DesiredStateError::Persistence(error.to_string()));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProposalSource {
    pub agent_id: String,
    pub task_id: String,
}

impl ProposalSource {
    #[must_use]
    pub fn new(agent_id: impl Into<String>, task_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            task_id: task_id.into(),
        }
    }

    fn validate(&self) -> Result<(), ProposalError> {
        for (label, value) in [("agent_id", &self.agent_id), ("task_id", &self.task_id)] {
            if value.trim().is_empty() || value.chars().any(char::is_whitespace) {
                return Err(ProposalError::Validation {
                    reason: format!("proposal {label} must be a non-empty opaque identifier"),
                });
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn audit_label(&self) -> String {
        format!("agent:{} task:{}", self.agent_id, self.task_id)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DesiredStateProposal {
    pub proposal_id: String,
    pub base_revision: u64,
    pub operations: Vec<DesiredStateMutation>,
    pub source: ProposalSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

impl DesiredStateProposal {
    #[must_use]
    pub fn new(
        proposal_id: impl Into<String>,
        base_revision: u64,
        operations: Vec<DesiredStateMutation>,
        source: ProposalSource,
    ) -> Self {
        Self {
            proposal_id: proposal_id.into(),
            base_revision,
            operations,
            source,
            idempotency_key: None,
        }
    }

    #[must_use]
    pub fn with_idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = Some(key.into());
        self
    }

    fn mutation(&self) -> DesiredStateMutation {
        DesiredStateMutation::Batch(self.operations.clone())
    }

    fn validate(&self) -> Result<(), ProposalError> {
        if self.proposal_id.trim().is_empty() {
            return Err(ProposalError::Validation {
                reason: "proposal id must not be empty".to_owned(),
            });
        }
        if self.operations.is_empty() {
            return Err(ProposalError::Validation {
                reason: "proposal must contain at least one operation".to_owned(),
            });
        }
        self.source.validate()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProposalPreview {
    pub proposal_id: String,
    pub base_revision: u64,
    pub affected_components: Vec<ComponentId>,
    pub predicted_restarts: Vec<ComponentId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProposalAuditRecord {
    pub proposal_id: String,
    pub source: ProposalSource,
    pub base_revision: u64,
    pub resulting_revision: u64,
    pub affected_components: Vec<ComponentId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProposalCommit {
    pub receipt: DesiredStateCommit,
    pub audit: ProposalAuditRecord,
}

#[derive(Debug, Error)]
pub enum ProposalError {
    #[error(transparent)]
    DesiredState(#[from] DesiredStateError),
    #[error("proposal validation failed: {reason}")]
    Validation { reason: String },
    #[error("proposal cannot directly mutate runtime registries or lifecycle state")]
    DirectMutationDenied,
}

pub struct SelfModificationApi<'a> {
    store: &'a DesiredStateStore,
}

impl<'a> SelfModificationApi<'a> {
    #[must_use]
    pub fn new(store: &'a DesiredStateStore) -> Self {
        Self { store }
    }

    pub fn preview(
        &self,
        proposal: &DesiredStateProposal,
        runtime: &ComponentRuntime,
    ) -> Result<ProposalPreview, ProposalError> {
        proposal.validate()?;
        let current = self.store.snapshot();
        if current.revision != proposal.base_revision {
            return Err(ProposalError::DesiredState(
                DesiredStateError::RevisionConflict {
                    expected: proposal.base_revision,
                    current,
                },
            ));
        }
        let mut next = current;
        proposal.mutation().apply(&mut next)?;
        let mut affected = proposal
            .operations
            .iter()
            .flat_map(mutation_components)
            .collect::<BTreeSet<_>>();
        affected.extend(
            next.components
                .keys()
                .filter(|component| !runtime.active_generations(component.as_str()).is_empty())
                .cloned(),
        );
        let mut predicted_restarts = Vec::new();
        for component in &affected {
            let active = runtime.active_generations(component.as_str());
            if active
                .iter()
                .any(|identity| runtime.spec(identity) != next.components.get(component))
            {
                predicted_restarts.push(component.clone());
            }
        }
        Ok(ProposalPreview {
            proposal_id: proposal.proposal_id.clone(),
            base_revision: proposal.base_revision,
            affected_components: affected.into_iter().collect(),
            predicted_restarts,
        })
    }

    pub fn commit(
        &self,
        proposal: &DesiredStateProposal,
        runtime: &ComponentRuntime,
    ) -> Result<ProposalCommit, ProposalError> {
        let preview = self.preview(proposal, runtime)?;
        let receipt = self.store.compare_and_swap_with_idempotency(
            proposal.base_revision,
            proposal.mutation(),
            proposal.source.audit_label(),
            proposal.idempotency_key.clone(),
        )?;
        let audit = ProposalAuditRecord {
            proposal_id: proposal.proposal_id.clone(),
            source: proposal.source.clone(),
            base_revision: proposal.base_revision,
            resulting_revision: receipt.revision,
            affected_components: preview.affected_components,
        };
        self.store.record_audit(audit.clone());
        Ok(ProposalCommit { receipt, audit })
    }

    pub fn apply(
        &self,
        runtime: &mut ComponentRuntime,
        commit: &ProposalCommit,
    ) -> Result<ReconcileReport, ProposalError> {
        Reconciler::reconcile(runtime, &commit.receipt.snapshot).map_err(ProposalError::from)
    }
}

pub struct AgentRuntimeFacade<'a> {
    proposals: SelfModificationApi<'a>,
}

impl<'a> AgentRuntimeFacade<'a> {
    #[must_use]
    pub fn new(store: &'a DesiredStateStore) -> Self {
        Self {
            proposals: SelfModificationApi::new(store),
        }
    }

    pub fn preview(
        &self,
        proposal: &DesiredStateProposal,
        runtime: &ComponentRuntime,
    ) -> Result<ProposalPreview, ProposalError> {
        self.proposals.preview(proposal, runtime)
    }

    pub fn commit(
        &self,
        proposal: &DesiredStateProposal,
        runtime: &ComponentRuntime,
    ) -> Result<ProposalCommit, ProposalError> {
        self.proposals.commit(proposal, runtime)
    }

    pub fn apply(
        &self,
        runtime: &mut ComponentRuntime,
        commit: &ProposalCommit,
    ) -> Result<ReconcileReport, ProposalError> {
        self.proposals.apply(runtime, commit)
    }
}

fn mutation_components(mutation: &DesiredStateMutation) -> Vec<ComponentId> {
    match mutation {
        DesiredStateMutation::Upsert(spec) => vec![spec.id.clone()],
        DesiredStateMutation::Remove(component)
        | DesiredStateMutation::SetEnabled { component, .. } => vec![component.clone()],
        DesiredStateMutation::Batch(mutations) => {
            mutations.iter().flat_map(mutation_components).collect()
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReconcileAction {
    Added {
        component: ComponentId,
    },
    Updated {
        component: ComponentId,
    },
    Disabled {
        component: ComponentId,
    },
    Removed {
        component: ComponentId,
    },
    Noop {
        component: ComponentId,
    },
    Blocked {
        component: ComponentId,
        reason: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReconcileReport {
    pub desired_revision: u64,
    pub applied: bool,
    pub actions: Vec<ReconcileAction>,
}

pub struct Reconciler;

impl Reconciler {
    pub fn reconcile(
        runtime: &mut ComponentRuntime,
        desired: &DesiredRuntimeState,
    ) -> Result<ReconcileReport, DesiredStateError> {
        let mut actions = Vec::new();
        let mut applied = false;
        let desired_ids = desired.components.keys().cloned().collect::<BTreeSet<_>>();
        for (component, spec) in &desired.components {
            let active = runtime.active_generations(component.as_str());
            if !spec.enabled {
                if active.is_empty() {
                    actions.push(ReconcileAction::Noop {
                        component: component.clone(),
                    });
                } else {
                    for identity in active {
                        let report = runtime.deactivate(&identity).map_err(|error| {
                            DesiredStateError::Validation {
                                reason: error.to_string(),
                            }
                        })?;
                        if report.is_clean() {
                            actions.push(ReconcileAction::Disabled {
                                component: component.clone(),
                            });
                            applied = true;
                        } else {
                            actions.push(ReconcileAction::Blocked {
                                component: component.clone(),
                                reason: format!("cleanup debt: {:?}", report.cleanup_debt),
                            });
                        }
                    }
                }
                continue;
            }
            if active.is_empty() {
                let identity = runtime
                    .instantiate(spec.clone(), desired.revision)
                    .map_err(|error| DesiredStateError::Validation {
                        reason: error.to_string(),
                    })?;
                runtime
                    .activate(&identity)
                    .map_err(|error| DesiredStateError::Validation {
                        reason: error.to_string(),
                    })?;
                actions.push(ReconcileAction::Added {
                    component: component.clone(),
                });
                applied = true;
                continue;
            }
            let mut changed = false;
            for identity in active {
                if runtime.spec(&identity) != Some(spec) {
                    let replacement = runtime.replace_component(
                        &identity,
                        spec.clone(),
                        |_, _| Ok(()),
                        |_| Ok(()),
                        ReplacementOptions::default(),
                    );
                    match replacement {
                        Ok(_) => {
                            changed = true;
                            applied = true;
                        }
                        Err(error) => actions.push(ReconcileAction::Blocked {
                            component: component.clone(),
                            reason: error.to_string(),
                        }),
                    }
                }
            }
            if changed {
                actions.push(ReconcileAction::Updated {
                    component: component.clone(),
                });
            } else if !actions.iter().any(|action| {
                matches!(action, ReconcileAction::Blocked { component: existing, .. } if existing == component)
            }) {
                actions.push(ReconcileAction::Noop {
                    component: component.clone(),
                });
            }
        }
        for identity in runtime.active_instances() {
            if !desired_ids.contains(identity.component_id()) {
                let report = runtime.deactivate(&identity).map_err(|error| {
                    DesiredStateError::Validation {
                        reason: error.to_string(),
                    }
                })?;
                if report.is_clean() {
                    actions.push(ReconcileAction::Removed {
                        component: identity.component_id.clone(),
                    });
                    applied = true;
                } else {
                    actions.push(ReconcileAction::Blocked {
                        component: identity.component_id.clone(),
                        reason: format!("cleanup debt: {:?}", report.cleanup_debt),
                    });
                }
            }
        }
        Ok(ReconcileReport {
            desired_revision: desired.revision,
            applied,
            actions,
        })
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
                        && candidate.available
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
    #[error(
        "external operation {operation_id:?} is irreversible and must be tracked by the commit ledger"
    )]
    ExternalCommitNotReversible { operation_id: String },
    #[error(
        "exclusive resource {resource:?} is owned by {owner:?}; cannot assign it to {requested:?}"
    )]
    ExclusiveResourceConflict {
        resource: String,
        owner: ComponentInstanceId,
        requested: ComponentInstanceId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeInvariantViolation {
    IdentityContextMismatch {
        identity: ComponentInstanceId,
        context_identity: ComponentInstanceId,
    },
    JournalOwnerMismatch {
        identity: ComponentInstanceId,
        journal_owner: ComponentInstanceId,
    },
    CapabilityDrift {
        identity: ComponentInstanceId,
    },
    RetiringInstanceActive {
        identity: ComponentInstanceId,
        state: LifecycleState,
    },
    RetiringStateMissingFlag {
        identity: ComponentInstanceId,
        state: LifecycleState,
    },
    OrphanedExclusiveOwner {
        resource: String,
        owner: ComponentInstanceId,
    },
    ExclusiveOwnershipDrift {
        resource: String,
        owner: ComponentInstanceId,
    },
    MissingExclusiveOwner {
        resource: String,
        owner: ComponentInstanceId,
    },
    GenerationCounterBehind {
        component: ComponentId,
        counter: ComponentGeneration,
        instance: ComponentInstanceId,
    },
    UnknownDependencyProvider {
        consumer: ComponentInstanceId,
        provider: ComponentInstanceId,
    },
}

struct ComponentRecord {
    spec: ComponentSpec,
    context: ScopedComponentContext,
    state: LifecycleState,
    journal: EffectJournal,
    committed_dependency_view: DependencyView,
    target_dependency_view: DependencyView,
    retiring: bool,
}

#[derive(Default)]
pub struct ComponentRuntime {
    next_generations: BTreeMap<ComponentId, ComponentGeneration>,
    instances: BTreeMap<ComponentInstanceId, ComponentRecord>,
    exclusive_owners: BTreeMap<String, ComponentInstanceId>,
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
        for resource in &spec.exclusive_resources {
            if let Some(owner) = self.exclusive_owners.get(resource) {
                return Err(ComponentRuntimeError::ExclusiveResourceConflict {
                    resource: resource.clone(),
                    owner: owner.clone(),
                    requested: identity.clone(),
                });
            }
        }
        for resource in &spec.exclusive_resources {
            self.exclusive_owners
                .insert(resource.clone(), identity.clone());
        }
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
                journal: EffectJournal::new(identity.clone()),
                committed_dependency_view: DependencyView::default(),
                target_dependency_view: DependencyView::default(),
                retiring: false,
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

    pub fn activate(
        &mut self,
        identity: &ComponentInstanceId,
    ) -> Result<(), ComponentRuntimeError> {
        let record = self
            .instances
            .get_mut(identity)
            .ok_or_else(|| ComponentRuntimeError::UnknownInstance(identity.clone()))?;
        if record.retiring {
            return Err(ComponentRuntimeError::InvalidSpec {
                component: identity.component_id.clone(),
                reason: "retiring components cannot be activated".to_owned(),
            });
        }
        record.state = LifecycleState::Active;
        Ok(())
    }

    pub fn deactivate(
        &mut self,
        identity: &ComponentInstanceId,
    ) -> Result<RollbackReport, ComponentRuntimeError> {
        let record = self
            .instances
            .get_mut(identity)
            .ok_or_else(|| ComponentRuntimeError::UnknownInstance(identity.clone()))?;
        record.state = LifecycleState::Deactivating;
        let report = record.journal.rollback();
        if report.is_clean() {
            record.state = LifecycleState::Inactive;
            record.committed_dependency_view = DependencyView::default();
            record.target_dependency_view = DependencyView::default();
        } else {
            record.state = LifecycleState::BlockedRetirement;
        }
        Ok(report)
    }

    pub fn record_effect<F>(
        &mut self,
        identity: &ComponentInstanceId,
        label: impl Into<String>,
        inverse: F,
    ) -> Result<u64, ComponentRuntimeError>
    where
        F: FnMut() -> Result<(), String> + Send + 'static,
    {
        let record = self
            .instances
            .get_mut(identity)
            .ok_or_else(|| ComponentRuntimeError::UnknownInstance(identity.clone()))?;
        Ok(record.journal.record_successful_effect(label, inverse))
    }

    pub fn provider_candidate(
        &self,
        identity: &ComponentInstanceId,
    ) -> Result<ProviderCandidate, ComponentRuntimeError> {
        let record = self
            .instances
            .get(identity)
            .ok_or_else(|| ComponentRuntimeError::UnknownInstance(identity.clone()))?;
        Ok(
            ProviderCandidate::new(identity.clone(), record.spec.clone())
                .retiring(record.retiring)
                .available(record.state == LifecycleState::Active),
        )
    }

    pub fn set_committed_dependency_view(
        &mut self,
        identity: &ComponentInstanceId,
        view: DependencyView,
    ) -> Result<(), ComponentRuntimeError> {
        let record = self
            .instances
            .get_mut(identity)
            .ok_or_else(|| ComponentRuntimeError::UnknownInstance(identity.clone()))?;
        record.committed_dependency_view = view.clone();
        record.target_dependency_view = view;
        Ok(())
    }

    #[must_use]
    pub fn committed_dependency_view(
        &self,
        identity: &ComponentInstanceId,
    ) -> Option<DependencyView> {
        self.instances
            .get(identity)
            .map(|record| record.committed_dependency_view.clone())
    }

    #[must_use]
    pub fn target_dependency_view(&self, identity: &ComponentInstanceId) -> Option<DependencyView> {
        self.instances
            .get(identity)
            .map(|record| record.target_dependency_view.clone())
    }

    #[must_use]
    pub fn is_provider_retiring(&self, identity: &ComponentInstanceId) -> bool {
        self.instances
            .get(identity)
            .is_some_and(|record| record.retiring)
    }

    pub fn dependency_reconciliation_plan(
        &self,
    ) -> Result<Vec<DependencyReconciliationAction>, DependencyResolutionError> {
        let candidates = self
            .instances
            .values()
            .map(|record| {
                ProviderCandidate::new(record.context.identity(), record.spec.clone())
                    .retiring(record.retiring)
                    .available(record.state == LifecycleState::Active)
            })
            .collect::<Vec<_>>();
        let mut actions = Vec::new();
        for (identity, record) in &self.instances {
            if record.state != LifecycleState::Active || record.retiring {
                continue;
            }
            match DependencyResolver::resolve(&record.spec, &candidates) {
                Ok(target) if target == record.committed_dependency_view => {
                    actions.push(DependencyReconciliationAction::Noop {
                        component: identity.clone(),
                    });
                }
                Ok(target) => actions.push(DependencyReconciliationAction::Restart {
                    component: identity.clone(),
                    committed: record.committed_dependency_view.clone(),
                    target,
                }),
                Err(error) => actions.push(DependencyReconciliationAction::Deactivate {
                    component: identity.clone(),
                    reason: error.to_string(),
                }),
            }
        }
        Ok(actions)
    }

    pub fn retire_provider(
        &mut self,
        provider: &ComponentInstanceId,
    ) -> Result<ProviderRetirementReport, ComponentRuntimeError> {
        let provider_record = self
            .instances
            .get_mut(provider)
            .ok_or_else(|| ComponentRuntimeError::UnknownInstance(provider.clone()))?;
        provider_record.retiring = true;
        provider_record.state = LifecycleState::Retiring;

        let consumers = self
            .instances
            .iter()
            .filter(|(identity, record)| {
                *identity != provider
                    && record.state == LifecycleState::Active
                    && record
                        .committed_dependency_view
                        .providers
                        .values()
                        .any(|providers| providers.contains(provider))
            })
            .map(|(identity, _)| identity.clone())
            .collect::<Vec<_>>();
        let mut report = ProviderRetirementReport {
            provider: provider.clone(),
            consumers: consumers.clone(),
            order: Vec::new(),
            blocked: Vec::new(),
            withdrawn: false,
        };

        for consumer in consumers {
            let record = self
                .instances
                .get_mut(&consumer)
                .expect("consumer collected from instances");
            record.state = LifecycleState::Deactivating;
            let rollback = record.journal.rollback();
            if !rollback.is_clean() {
                record.state = LifecycleState::BlockedRetirement;
                report.blocked.push(consumer);
                if let Some(provider_record) = self.instances.get_mut(provider) {
                    provider_record.state = LifecycleState::BlockedRetirement;
                }
                return Ok(report);
            }
            record.state = LifecycleState::Inactive;
            record.committed_dependency_view = DependencyView::default();
            record.target_dependency_view = DependencyView::default();
            report.order.push(consumer);
        }

        let provider_rollback = self
            .instances
            .get_mut(provider)
            .expect("provider exists")
            .journal
            .rollback();
        if !provider_rollback.is_clean() {
            if let Some(provider_record) = self.instances.get_mut(provider) {
                provider_record.state = LifecycleState::BlockedRetirement;
            }
            report.blocked.push(provider.clone());
            return Ok(report);
        }
        if let Some(provider_record) = self.instances.get_mut(provider) {
            provider_record.state = LifecycleState::Inactive;
        }
        self.exclusive_owners.retain(|_, owner| owner != provider);
        report.order.push(provider.clone());
        report.withdrawn = true;
        Ok(report)
    }

    pub fn effect_pending(
        &self,
        identity: &ComponentInstanceId,
    ) -> Result<bool, ComponentRuntimeError> {
        self.instances
            .get(identity)
            .map(|record| record.journal.pending_effect_count() > 0)
            .ok_or_else(|| ComponentRuntimeError::UnknownInstance(identity.clone()))
    }

    #[must_use]
    pub fn invariant_violations(&self) -> Vec<RuntimeInvariantViolation> {
        let mut violations = Vec::new();
        for (identity, record) in &self.instances {
            if &record.context.identity != identity {
                violations.push(RuntimeInvariantViolation::IdentityContextMismatch {
                    identity: identity.clone(),
                    context_identity: record.context.identity.clone(),
                });
            }
            if record.journal.owner() != identity {
                violations.push(RuntimeInvariantViolation::JournalOwnerMismatch {
                    identity: identity.clone(),
                    journal_owner: record.journal.owner().clone(),
                });
            }
            if record.context.capabilities != record.spec.capabilities {
                violations.push(RuntimeInvariantViolation::CapabilityDrift {
                    identity: identity.clone(),
                });
            }
            if record.retiring
                && matches!(
                    record.state,
                    LifecycleState::Active | LifecycleState::Activating
                )
            {
                violations.push(RuntimeInvariantViolation::RetiringInstanceActive {
                    identity: identity.clone(),
                    state: record.state,
                });
            }
            if !record.retiring
                && matches!(
                    record.state,
                    LifecycleState::Retiring | LifecycleState::BlockedRetirement
                )
            {
                violations.push(RuntimeInvariantViolation::RetiringStateMissingFlag {
                    identity: identity.clone(),
                    state: record.state,
                });
            }
            if self
                .next_generations
                .get(&identity.component_id)
                .is_none_or(|counter| counter.get() < identity.generation.get())
            {
                violations.push(RuntimeInvariantViolation::GenerationCounterBehind {
                    component: identity.component_id.clone(),
                    counter: self
                        .next_generations
                        .get(&identity.component_id)
                        .copied()
                        .unwrap_or(ComponentGeneration::new(0)),
                    instance: identity.clone(),
                });
            }
            for view in [
                &record.committed_dependency_view,
                &record.target_dependency_view,
            ] {
                for provider in view.providers.values().flatten() {
                    if !self.instances.contains_key(provider) {
                        violations.push(RuntimeInvariantViolation::UnknownDependencyProvider {
                            consumer: identity.clone(),
                            provider: provider.clone(),
                        });
                    }
                }
            }
            for resource in &record.spec.exclusive_resources {
                if self.exclusive_owners.get(resource) != Some(identity) {
                    violations.push(RuntimeInvariantViolation::MissingExclusiveOwner {
                        resource: resource.clone(),
                        owner: identity.clone(),
                    });
                }
            }
        }
        for (resource, owner) in &self.exclusive_owners {
            let Some(record) = self.instances.get(owner) else {
                violations.push(RuntimeInvariantViolation::OrphanedExclusiveOwner {
                    resource: resource.clone(),
                    owner: owner.clone(),
                });
                continue;
            };
            if !record.spec.exclusive_resources.contains(resource) {
                violations.push(RuntimeInvariantViolation::ExclusiveOwnershipDrift {
                    resource: resource.clone(),
                    owner: owner.clone(),
                });
            }
        }
        violations
    }

    pub fn validate_invariants(&self) -> Result<(), Vec<RuntimeInvariantViolation>> {
        let violations = self.invariant_violations();
        if violations.is_empty() {
            Ok(())
        } else {
            Err(violations)
        }
    }

    #[must_use]
    pub fn contains(&self, identity: &ComponentInstanceId) -> bool {
        self.instances.contains_key(identity)
    }

    #[must_use]
    pub fn active_generations(&self, component_id: &str) -> Vec<ComponentInstanceId> {
        self.instances
            .iter()
            .filter(|(identity, record)| {
                identity.component_id.as_str() == component_id
                    && record.state == LifecycleState::Active
            })
            .map(|(identity, _)| identity.clone())
            .collect()
    }

    #[must_use]
    pub fn active_instances(&self) -> Vec<ComponentInstanceId> {
        self.instances
            .iter()
            .filter(|(_, record)| record.state == LifecycleState::Active)
            .map(|(identity, _)| identity.clone())
            .collect()
    }

    pub fn replace_component<A, H>(
        &mut self,
        old: &ComponentInstanceId,
        candidate_spec: ComponentSpec,
        activate: A,
        health_check: H,
        options: ReplacementOptions,
    ) -> Result<ReplacementOutcome, ReplacementError>
    where
        A: FnOnce(&ScopedComponentContext, &mut EffectJournal) -> Result<(), String>,
        H: FnOnce(&ScopedComponentContext) -> Result<(), String>,
    {
        let started = Instant::now();
        options.check(started)?;
        let old_record = self
            .instances
            .get(old)
            .ok_or_else(|| ComponentRuntimeError::UnknownInstance(old.clone()))?;
        if candidate_spec.id != old.component_id {
            return Err(ReplacementError::ComponentIdMismatch);
        }
        let desired_revision = old_record.context.desired_revision();
        let candidate = self.instantiate(candidate_spec, desired_revision)?;
        let candidate_context = self.context(&candidate)?;
        if let Err(control) = options.check(started) {
            return self.finish_candidate_control_failure(&candidate, control);
        }

        let activation_error = {
            let record = self
                .instances
                .get_mut(&candidate)
                .expect("candidate exists after instantiate");
            record.state = LifecycleState::Activating;
            activate(&candidate_context, &mut record.journal).err()
        };
        if let Some(reason) = activation_error {
            return Err(self.reject_candidate(&candidate, reason));
        }
        if let Err(control) = options.check(started) {
            return self.finish_candidate_control_failure(&candidate, control);
        }
        if let Err(reason) = health_check(&candidate_context) {
            return Err(self.reject_candidate(&candidate, reason));
        }
        if let Err(control) = options.check(started) {
            return self.finish_candidate_control_failure(&candidate, control);
        }

        if let Some(record) = self.instances.get_mut(&candidate) {
            record.state = LifecycleState::Active;
        }
        let candidate_providers = self
            .instances
            .values()
            .map(|record| {
                ProviderCandidate::new(record.context.identity(), record.spec.clone())
                    .retiring(record.retiring)
                    .available(record.state == LifecycleState::Active)
            })
            .collect::<Vec<_>>();
        let candidate_target = {
            let spec = self
                .instances
                .get(&candidate)
                .expect("candidate exists")
                .spec
                .clone();
            DependencyResolver::resolve(&spec, &candidate_providers)
        };
        let candidate_target = match candidate_target {
            Ok(target) => target,
            Err(reason) => return Err(self.reject_candidate(&candidate, reason.to_string())),
        };
        if let Some(record) = self.instances.get_mut(&candidate) {
            record.committed_dependency_view = candidate_target.clone();
            record.target_dependency_view = candidate_target;
        }

        let consumers = self
            .instances
            .iter()
            .filter(|(identity, record)| {
                *identity != old
                    && *identity != &candidate
                    && record.state == LifecycleState::Active
                    && record
                        .committed_dependency_view
                        .providers
                        .values()
                        .any(|providers| providers.contains(old))
            })
            .map(|(identity, _)| identity.clone())
            .collect::<Vec<_>>();
        let saved_consumers = consumers
            .iter()
            .filter_map(|identity| {
                self.instances.get(identity).map(|record| {
                    (
                        identity.clone(),
                        record.state,
                        record.committed_dependency_view.clone(),
                        record.target_dependency_view.clone(),
                    )
                })
            })
            .collect::<Vec<_>>();
        let mut migrated_consumers = Vec::new();
        for consumer in &consumers {
            let record = self
                .instances
                .get_mut(consumer)
                .expect("consumer collected from instances");
            record.state = LifecycleState::Deactivating;
            let rollback = record.journal.rollback();
            if !rollback.is_clean() {
                self.restore_consumers(&saved_consumers);
                return Err(self.reject_candidate(
                    &candidate,
                    format!(
                        "consumer {} teardown blocked: {:?}",
                        consumer.component_id.as_str(),
                        rollback.cleanup_debt
                    ),
                ));
            }
            let mut replacement_view = record.committed_dependency_view.clone();
            for providers in replacement_view.providers.values_mut() {
                for provider in providers {
                    if provider == old {
                        *provider = candidate.clone();
                    }
                }
            }
            record.committed_dependency_view = replacement_view.clone();
            record.target_dependency_view = replacement_view;
            record.state = LifecycleState::Active;
            migrated_consumers.push(consumer.clone());
        }

        let old_rollback = self
            .instances
            .get_mut(old)
            .expect("old provider exists")
            .journal
            .rollback();
        if !old_rollback.is_clean() {
            self.restore_consumers(&saved_consumers);
            let _ = self.reject_candidate(&candidate, "old provider cleanup blocked".to_owned());
            if let Some(record) = self.instances.get_mut(old) {
                record.state = LifecycleState::BlockedRetirement;
                record.retiring = true;
            }
            return Err(ReplacementError::OldProviderCleanupBlocked {
                reason: format!("cleanup debt: {:?}", old_rollback.cleanup_debt),
            });
        }
        if let Some(record) = self.instances.get_mut(old) {
            record.retiring = true;
            record.state = LifecycleState::Inactive;
        }
        self.exclusive_owners.retain(|_, owner| owner != old);
        Ok(ReplacementOutcome {
            old: old.clone(),
            candidate,
            migrated_consumers,
            old_withdrawn: true,
        })
    }

    fn reject_candidate(
        &mut self,
        candidate: &ComponentInstanceId,
        reason: String,
    ) -> ReplacementError {
        let cleanup_debt = self
            .instances
            .get_mut(candidate)
            .map(|record| record.journal.rollback().cleanup_debt)
            .unwrap_or_default();
        self.remove_instance(candidate);
        ReplacementError::CandidateRejected {
            reason,
            cleanup_debt,
        }
    }

    fn finish_candidate_control_failure(
        &mut self,
        candidate: &ComponentInstanceId,
        control: ReplacementError,
    ) -> Result<ReplacementOutcome, ReplacementError> {
        let cleanup_debt = self
            .instances
            .get_mut(candidate)
            .map(|record| record.journal.rollback().cleanup_debt)
            .unwrap_or_default();
        self.remove_instance(candidate);
        if cleanup_debt.is_empty() {
            Err(control)
        } else {
            Err(ReplacementError::CandidateRejected {
                reason: control.to_string(),
                cleanup_debt,
            })
        }
    }

    fn restore_consumers(
        &mut self,
        saved: &[(
            ComponentInstanceId,
            LifecycleState,
            DependencyView,
            DependencyView,
        )],
    ) {
        for (identity, state, committed, target) in saved {
            if let Some(record) = self.instances.get_mut(identity) {
                record.state = *state;
                record.committed_dependency_view = committed.clone();
                record.target_dependency_view = target.clone();
            }
        }
    }

    fn remove_instance(&mut self, identity: &ComponentInstanceId) {
        self.instances.remove(identity);
        self.exclusive_owners.retain(|_, owner| owner != identity);
    }
}
