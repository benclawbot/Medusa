use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

use medusa_process_containment::{NativeProcessStartMarker, process_start_marker};
use medusa_process_registry::{
    IdentityVerification, ProcessId, ProcessIdentity, ProcessRecord, ProcessRegistry, ProcessSpec,
    ProcessStartMarker, ProcessState, RegistryError, REGISTRY_SCHEMA_VERSION,
};
use medusa_recovery_coordinator::{
    RecoveryAction, RecoveryCandidate, RecoveryCoordinator, RecoveryDecision, RecoveryError,
    TransactionPhase,
};
use medusa_runtime_supervisor::{Signal, SupervisorState};
use medusa_wakeup::{
    SubscriptionId, WakeupDelivery, WakeupEvent, WakeupRouter, WakeupSource, WakeupSubscription,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::{Duration, OffsetDateTime};

const CONTROL_SCHEMA_VERSION: u32 = 1;
const LEGACY_REGISTRY_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeBinding {
    pub execution_id: String,
    pub process_id: ProcessId,
    pub checkpoint_ref: Option<String>,
    pub shutdown_requested: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SupervisionEvent {
    RuntimeRegistered {
        execution_id: String,
        process_id: String,
        generation: u64,
        #[serde(default)]
        start_marker: Option<ProcessStartMarker>,
    },
    HeartbeatRecorded {
        execution_id: String,
        process_id: String,
        checkpoint_ref: Option<String>,
    },
    RecoveryDecided {
        execution_id: String,
        process_id: String,
        action: RecoveryAction,
        reason: String,
        evidence_fingerprint: String,
    },
    IdentityRejected {
        process_id: String,
        verification: IdentityVerification,
    },
    ForeignOwnershipIgnored {
        process_id: String,
    },
    ShutdownRecorded {
        execution_id: String,
        process_id: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DurableControlState {
    schema_version: u32,
    registry: ProcessRegistry,
    wakeups: WakeupRouter,
    bindings: BTreeMap<ProcessId, RuntimeBinding>,
    recovery_attempts: BTreeSet<String>,
    events: Vec<SupervisionEvent>,
    next_wakeup_sequence: u64,
}

impl Default for DurableControlState {
    fn default() -> Self {
        Self {
            schema_version: CONTROL_SCHEMA_VERSION,
            registry: ProcessRegistry::default(),
            wakeups: WakeupRouter::default(),
            bindings: BTreeMap::new(),
            recovery_attempts: BTreeSet::new(),
            events: Vec::new(),
            next_wakeup_sequence: 1,
        }
    }
}

pub struct SupervisionControlPlane {
    path: PathBuf,
    installation_session: String,
    state: DurableControlState,
}

impl SupervisionControlPlane {
    pub fn load(
        path: impl Into<PathBuf>,
        installation_session: impl Into<String>,
    ) -> Result<Self, ControlPlaneError> {
        let path = path.into();
        let installation_session = installation_session.into();
        if installation_session.trim().is_empty() {
            return Err(ControlPlaneError::InvalidInstallationSession);
        }
        let state = if path.exists() {
            let bytes = fs::read(&path)?;
            let mut value: Value = serde_json::from_slice(&bytes)?;
            migrate_embedded_registry(&mut value)?;
            let state: DurableControlState = serde_json::from_value(value)?;
            validate_state(&state)?;
            state
        } else {
            DurableControlState::default()
        };
        Ok(Self {
            path,
            installation_session,
            state,
        })
    }

    pub fn register_runtime(
        &mut self,
        execution_id: impl Into<String>,
        process_id: ProcessId,
        spec: ProcessSpec,
        pid: u32,
        checkpoint_ref: Option<String>,
        now: OffsetDateTime,
    ) -> Result<(), ControlPlaneError> {
        let execution_id = execution_id.into();
        if execution_id.trim().is_empty() {
            return Err(ControlPlaneError::InvalidExecutionId);
        }
        let start_marker = acquire_start_marker(pid)?;
        let mut record = ProcessRecord::new(
            process_id.clone(),
            spec,
            now,
            Some(self.installation_session.clone()),
        )?;
        record.mark_running_with_marker(pid, Some(start_marker.clone()), now)?;
        let generation = record.generation;
        self.state.registry.register(record)?;
        self.state.bindings.insert(
            process_id.clone(),
            RuntimeBinding {
                execution_id: execution_id.clone(),
                process_id: process_id.clone(),
                checkpoint_ref,
                shutdown_requested: false,
            },
        );
        let subscription_id =
            SubscriptionId::parse(format!("recover-{}-{generation}", process_id.as_str()))
                .map_err(ControlPlaneError::Wakeup)?;
        self.state
            .wakeups
            .subscribe(WakeupSubscription {
                id: subscription_id,
                owner: execution_id.clone(),
                source: WakeupSource::ProcessOrphaned(process_id.as_str().to_owned()),
                one_shot: true,
                enabled: true,
            })
            .map_err(ControlPlaneError::Wakeup)?;
        self.state.events.push(SupervisionEvent::RuntimeRegistered {
            execution_id,
            process_id: process_id.as_str().to_owned(),
            generation,
            start_marker: Some(start_marker),
        });
        self.persist()
    }

    pub fn heartbeat(
        &mut self,
        process_id: &ProcessId,
        checkpoint_ref: Option<String>,
        now: OffsetDateTime,
    ) -> Result<(), ControlPlaneError> {
        self.state.registry.get_mut(process_id)?.heartbeat(now)?;
        let binding = self
            .state
            .bindings
            .get_mut(process_id)
            .ok_or_else(|| ControlPlaneError::MissingBinding(process_id.as_str().to_owned()))?;
        binding.checkpoint_ref = checkpoint_ref.clone();
        self.state.events.push(SupervisionEvent::HeartbeatRecorded {
            execution_id: binding.execution_id.clone(),
            process_id: process_id.as_str().to_owned(),
            checkpoint_ref,
        });
        self.persist()
    }

    pub fn request_shutdown(
        &mut self,
        process_id: &ProcessId,
        now: OffsetDateTime,
    ) -> Result<(), ControlPlaneError> {
        let record = self.state.registry.get_mut(process_id)?;
        let verification = verify_native_identity(record.identity.as_ref());
        record.last_identity_verification = Some(verification);
        if !verification.permits_destructive_action() {
            return Err(ControlPlaneError::UnsafeProcessIdentity {
                process_id: process_id.as_str().to_owned(),
                verification,
            });
        }
        record.transition(ProcessState::Stopping, now)?;
        let binding = self
            .state
            .bindings
            .get_mut(process_id)
            .ok_or_else(|| ControlPlaneError::MissingBinding(process_id.as_str().to_owned()))?;
        binding.shutdown_requested = true;
        self.state.events.push(SupervisionEvent::ShutdownRecorded {
            execution_id: binding.execution_id.clone(),
            process_id: process_id.as_str().to_owned(),
        });
        self.persist()
    }

    pub fn reconcile(
        &mut self,
        now: OffsetDateTime,
        heartbeat_timeout: Duration,
        is_alive: impl Fn(u32) -> bool,
    ) -> Result<Vec<RecoveryDecision>, ControlPlaneError> {
        let orphaned = self.state.registry.reconcile_with_identity(
            now,
            heartbeat_timeout,
            |identity| {
                if is_alive(identity.pid) {
                    verify_native_identity(Some(identity))
                } else {
                    IdentityVerification::ProcessMissing
                }
            },
        );
        let mut decisions = Vec::new();
        for process_id in orphaned {
            let record = self
                .state
                .registry
                .get(&process_id)
                .ok_or_else(|| ControlPlaneError::MissingBinding(process_id.as_str().to_owned()))?
                .clone();
            if let Some(verification) = record.last_identity_verification
                && verification != IdentityVerification::ProcessMissing
            {
                self.state.events.push(SupervisionEvent::IdentityRejected {
                    process_id: process_id.as_str().to_owned(),
                    verification,
                });
            }
            if record.owner_session.as_deref() != Some(self.installation_session.as_str()) {
                self.state
                    .events
                    .push(SupervisionEvent::ForeignOwnershipIgnored {
                        process_id: process_id.as_str().to_owned(),
                    });
                continue;
            }
            let attempt_key = format!("{}:{}", process_id.as_str(), record.generation);
            if !self.state.recovery_attempts.insert(attempt_key) {
                continue;
            }
            let binding = self
                .state
                .bindings
                .get(&process_id)
                .ok_or_else(|| ControlPlaneError::MissingBinding(process_id.as_str().to_owned()))?
                .clone();
            let deliveries = self.route_orphaned(&process_id, now)?;
            if deliveries.is_empty() {
                continue;
            }
            let decision = decide_recovery(&record, &binding)?;
            let mut supervisor = SupervisorState::new(binding.execution_id.clone())
                .map_err(ControlPlaneError::Supervisor)?;
            match decision.action {
                RecoveryAction::Quarantine | RecoveryAction::NoOp => supervisor
                    .apply(Signal::TerminalFailure {
                        reason: decision.reason.clone(),
                    })
                    .map_err(ControlPlaneError::Supervisor)?,
                _ => supervisor
                    .apply(Signal::RecoveryRequired {
                        reason: decision.reason.clone(),
                    })
                    .map_err(ControlPlaneError::Supervisor)?,
            }
            self.state.events.push(SupervisionEvent::RecoveryDecided {
                execution_id: binding.execution_id,
                process_id: process_id.as_str().to_owned(),
                action: decision.action.clone(),
                reason: decision.reason.clone(),
                evidence_fingerprint: decision.evidence_fingerprint.clone(),
            });
            decisions.push(decision);
        }
        self.persist()?;
        Ok(decisions)
    }

    #[must_use]
    pub fn events(&self) -> &[SupervisionEvent] {
        &self.state.events
    }

    #[must_use]
    pub fn binding(&self, process_id: &ProcessId) -> Option<&RuntimeBinding> {
        self.state.bindings.get(process_id)
    }

    fn route_orphaned(
        &mut self,
        process_id: &ProcessId,
        now: OffsetDateTime,
    ) -> Result<Vec<WakeupDelivery>, ControlPlaneError> {
        let sequence = self.state.next_wakeup_sequence;
        let deliveries = self
            .state
            .wakeups
            .route(WakeupEvent {
                sequence,
                occurred_at: now,
                source: WakeupSource::ProcessOrphaned(process_id.as_str().to_owned()),
                metadata: BTreeMap::new(),
            })
            .map_err(ControlPlaneError::Wakeup)?;
        self.state.next_wakeup_sequence = sequence
            .checked_add(1)
            .ok_or(ControlPlaneError::WakeupSequenceOverflow)?;
        Ok(deliveries)
    }

    fn persist(&self) -> Result<(), ControlPlaneError> {
        validate_state(&self.state)?;
        let parent = self
            .path
            .parent()
            .ok_or(ControlPlaneError::MissingParentDirectory)?;
        fs::create_dir_all(parent)?;
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(&self.state)?)?;
        fs::rename(temporary, &self.path)?;
        Ok(())
    }
}

fn acquire_start_marker(pid: u32) -> Result<ProcessStartMarker, ControlPlaneError> {
    let native = process_start_marker(pid)?
        .ok_or(ControlPlaneError::ProcessIdentityUnavailable(pid))?;
    ProcessStartMarker::new(native.platform, native.value, native.boot_id).map_err(Into::into)
}

fn observed_marker(native: NativeProcessStartMarker) -> ProcessStartMarker {
    ProcessStartMarker {
        platform: native.platform.to_owned(),
        value: native.value,
        boot_id: native.boot_id,
    }
}

fn verify_native_identity(identity: Option<&ProcessIdentity>) -> IdentityVerification {
    let Some(identity) = identity else {
        return IdentityVerification::IdentityUnavailable;
    };
    if identity.start_marker.is_none() {
        return IdentityVerification::IdentityUnavailable;
    }
    match process_start_marker(identity.pid) {
        Ok(Some(native)) => {
            let observed = observed_marker(native);
            identity.verify_start_marker(Some(&observed))
        }
        Ok(None) => IdentityVerification::ProcessMissing,
        Err(_) => IdentityVerification::IdentityUnavailable,
    }
}

fn decide_recovery(
    record: &ProcessRecord,
    binding: &RuntimeBinding,
) -> Result<RecoveryDecision, ControlPlaneError> {
    let checkpoint = binding
        .checkpoint_ref
        .clone()
        .unwrap_or_else(|| digest("no-checkpoint"));
    let candidate = RecoveryCandidate {
        transaction_id: format!("runtime-{}-{}", record.id.as_str(), record.generation),
        execution_id: binding.execution_id.clone(),
        phase: if record.spec.restartable {
            TransactionPhase::Committing
        } else {
            TransactionPhase::Failed
        },
        checkpoint_sequence: record.generation,
        checkpoint_fingerprint: normalize_digest(&checkpoint),
        snapshot_fingerprint: digest(&format!("snapshot:{}", record.id.as_str())),
        replay_fingerprint: digest(&format!("replay:{}", record.generation)),
        rollback_fingerprint: Some(digest(&format!("rollback:{}", record.id.as_str()))),
    };
    let mut coordinator = RecoveryCoordinator::default();
    let lock = coordinator.acquire_lock(
        candidate.transaction_id.clone(),
        "daemon-control-plane",
        record.generation,
    )?;
    Ok(coordinator.decide(&candidate, &lock)?)
}

fn normalize_digest(value: &str) -> String {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        value.to_ascii_lowercase()
    } else {
        digest(value)
    }
}

fn digest(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn migrate_embedded_registry(value: &mut Value) -> Result<(), ControlPlaneError> {
    let Some(root) = value.as_object_mut() else {
        return Err(ControlPlaneError::InvalidDurableState);
    };
    let Some(registry) = root.get_mut("registry").and_then(Value::as_object_mut) else {
        return Err(ControlPlaneError::InvalidDurableState);
    };
    let schema_version = registry
        .get("schema_version")
        .and_then(Value::as_u64)
        .ok_or(ControlPlaneError::InvalidDurableState)? as u32;
    if schema_version == REGISTRY_SCHEMA_VERSION {
        return Ok(());
    }
    if schema_version != LEGACY_REGISTRY_SCHEMA_VERSION {
        return Err(ControlPlaneError::UnsupportedRegistrySchema(schema_version));
    }
    let Some(records) = registry.get_mut("records").and_then(Value::as_object_mut) else {
        return Err(ControlPlaneError::InvalidDurableState);
    };
    for record in records.values_mut() {
        let Some(record) = record.as_object_mut() else {
            return Err(ControlPlaneError::InvalidDurableState);
        };
        let pid = record
            .get("pid")
            .and_then(Value::as_u64)
            .map(|pid| pid as u32);
        let generation = record
            .get("generation")
            .and_then(Value::as_u64)
            .ok_or(ControlPlaneError::InvalidDurableState)?;
        if let Some(pid) = pid {
            record.insert(
                "identity".to_owned(),
                serde_json::json!({
                    "pid": pid,
                    "generation": generation,
                    "start_marker": null
                }),
            );
            record.insert(
                "last_identity_verification".to_owned(),
                Value::String("identity_unavailable".to_owned()),
            );
        }
    }
    registry.insert(
        "schema_version".to_owned(),
        Value::from(REGISTRY_SCHEMA_VERSION),
    );
    Ok(())
}

fn validate_state(state: &DurableControlState) -> Result<(), ControlPlaneError> {
    if state.schema_version != CONTROL_SCHEMA_VERSION {
        return Err(ControlPlaneError::UnsupportedSchema(state.schema_version));
    }
    state.registry.validate()?;
    state
        .wakeups
        .validate()
        .map_err(ControlPlaneError::Wakeup)?;
    for (process_id, binding) in &state.bindings {
        if process_id != &binding.process_id || binding.execution_id.trim().is_empty() {
            return Err(ControlPlaneError::InvalidBinding);
        }
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ControlPlaneError {
    #[error("installation session cannot be empty")]
    InvalidInstallationSession,
    #[error("execution id cannot be empty")]
    InvalidExecutionId,
    #[error("invalid runtime binding")]
    InvalidBinding,
    #[error("invalid durable supervision state")]
    InvalidDurableState,
    #[error("missing runtime binding for process {0}")]
    MissingBinding(String),
    #[error("process {0} has no native start identity")]
    ProcessIdentityUnavailable(u32),
    #[error("unsafe process identity for {process_id}: {verification:?}")]
    UnsafeProcessIdentity {
        process_id: String,
        verification: IdentityVerification,
    },
    #[error("unsupported supervision schema version {0}")]
    UnsupportedSchema(u32),
    #[error("unsupported embedded registry schema version {0}")]
    UnsupportedRegistrySchema(u32),
    #[error("supervision state path has no parent directory")]
    MissingParentDirectory,
    #[error("wakeup sequence overflow")]
    WakeupSequenceOverflow,
    #[error("wakeup policy error: {0}")]
    Wakeup(&'static str),
    #[error("supervisor state error: {0}")]
    Supervisor(&'static str),
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error(transparent)]
    Recovery(#[from] RecoveryError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use time::macros::datetime;

    fn process_id() -> ProcessId {
        ProcessId::parse("runtime-1").expect("process id")
    }

    fn current_pid() -> u32 {
        std::process::id()
    }

    fn spec(restartable: bool) -> ProcessSpec {
        ProcessSpec {
            program: "medusa-runtime".to_owned(),
            args: Vec::new(),
            working_directory: None,
            restartable,
        }
    }

    #[test]
    fn recovery_is_deduplicated_across_daemon_restart() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("supervision.json");
        let now = datetime!(2026-07-26 07:00 UTC);
        let mut plane = SupervisionControlPlane::load(&path, "install-a").expect("control plane");
        plane
            .register_runtime(
                "exec-1",
                process_id(),
                spec(true),
                current_pid(),
                None,
                now,
            )
            .expect("register");
        let first = plane
            .reconcile(now + Duration::minutes(10), Duration::minutes(5), |_| false)
            .expect("reconcile");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].action, RecoveryAction::ResumeCommit);

        let mut reloaded =
            SupervisionControlPlane::load(&path, "install-a").expect("reload control plane");
        let second = reloaded
            .reconcile(now + Duration::minutes(11), Duration::minutes(5), |_| false)
            .expect("reconcile after restart");
        assert!(second.is_empty());
    }

    #[test]
    fn stale_pid_from_another_installation_is_never_recovered() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("supervision.json");
        let now = datetime!(2026-07-26 07:00 UTC);
        let mut plane = SupervisionControlPlane::load(&path, "install-a").expect("control plane");
        plane
            .register_runtime(
                "exec-1",
                process_id(),
                spec(true),
                current_pid(),
                None,
                now,
            )
            .expect("register");

        let mut foreign =
            SupervisionControlPlane::load(&path, "install-b").expect("foreign control plane");
        let decisions = foreign
            .reconcile(now + Duration::minutes(10), Duration::minutes(5), |_| true)
            .expect("reconcile");
        assert!(decisions.is_empty());
        assert!(matches!(
            foreign.events().last(),
            Some(SupervisionEvent::ForeignOwnershipIgnored { .. })
        ));
    }

    #[test]
    fn terminal_processes_surface_quarantine_without_retrying() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("supervision.json");
        let now = datetime!(2026-07-26 07:00 UTC);
        let mut plane = SupervisionControlPlane::load(&path, "install-a").expect("control plane");
        plane
            .register_runtime(
                "exec-1",
                process_id(),
                spec(false),
                current_pid(),
                None,
                now,
            )
            .expect("register");
        let decisions = plane
            .reconcile(now + Duration::minutes(10), Duration::minutes(5), |_| false)
            .expect("reconcile");
        assert_eq!(decisions[0].action, RecoveryAction::Quarantine);
        assert!(decisions[0].reason.contains("operator"));
    }

    #[test]
    fn shutdown_reverifies_native_identity() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("supervision.json");
        let now = datetime!(2026-07-26 07:00 UTC);
        let mut plane = SupervisionControlPlane::load(&path, "install-a").expect("control plane");
        plane
            .register_runtime(
                "exec-1",
                process_id(),
                spec(true),
                current_pid(),
                None,
                now,
            )
            .expect("register");
        plane
            .request_shutdown(&process_id(), now + Duration::minutes(1))
            .expect("verified shutdown");
        assert!(matches!(
            plane.events().last(),
            Some(SupervisionEvent::ShutdownRecorded { .. })
        ));
    }
}
