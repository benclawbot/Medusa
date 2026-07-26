//! Durable daemon-owned supervision, wakeup, and recovery control plane.

use std::{collections::{BTreeMap, BTreeSet}, fs, path::{Path, PathBuf}};

use medusa_process_registry::{ProcessId, ProcessRecord, ProcessRegistry, ProcessState};
use medusa_recovery_coordinator::{RecoveryAction, RecoveryCandidate, RecoveryCoordinator, RecoveryDecision};
use medusa_runtime_supervisor::{Signal, SupervisorState};
use medusa_wakeup::{SubscriptionId, WakeupDelivery, WakeupEvent, WakeupRouter, WakeupSource, WakeupSubscription};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SupervisionEvent {
    ProcessRegistered { process_id: String, execution_id: String },
    HeartbeatRecorded { process_id: String, execution_id: String },
    ProcessOrphaned { process_id: String, execution_id: String, reason: String },
    RecoverySelected { execution_id: String, action: RecoveryAction, reason: String },
    TerminalActionRequired { execution_id: String, reason: String },
    RuntimeShutdown { process_id: String, execution_id: String, clean: bool },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct DurableState {
    registry: ProcessRegistry,
    wakeups: WakeupRouter,
    supervisors: BTreeMap<String, SupervisorState>,
    process_executions: BTreeMap<String, String>,
    recovered_executions: BTreeSet<String>,
    next_wakeup_sequence: u64,
    events: Vec<SupervisionEvent>,
}

impl Default for DurableState {
    fn default() -> Self {
        Self {
            registry: ProcessRegistry::default(),
            wakeups: WakeupRouter::default(),
            supervisors: BTreeMap::new(),
            process_executions: BTreeMap::new(),
            recovered_executions: BTreeSet::new(),
            next_wakeup_sequence: 1,
            events: Vec::new(),
        }
    }
}

pub struct ResilienceControlPlane {
    path: PathBuf,
    installation_id: String,
    state: DurableState,
    recovery: RecoveryCoordinator,
}

impl ResilienceControlPlane {
    pub fn load_or_create(path: impl Into<PathBuf>, installation_id: impl Into<String>) -> Result<Self, String> {
        let path = path.into();
        let installation_id = installation_id.into();
        if installation_id.trim().is_empty() {
            return Err("installation identifier cannot be empty".into());
        }
        let state = if path.exists() {
            serde_json::from_slice::<DurableState>(&fs::read(&path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?
        } else {
            DurableState::default()
        };
        state.registry.validate().map_err(|e| e.to_string())?;
        state.wakeups.validate().map_err(str::to_owned)?;
        for supervisor in state.supervisors.values() {
            supervisor.validate().map_err(str::to_owned)?;
        }
        Ok(Self { path, installation_id, state, recovery: RecoveryCoordinator::default() })
    }

    pub fn register(&mut self, record: ProcessRecord, execution_id: impl Into<String>) -> Result<(), String> {
        let execution_id = execution_id.into();
        if execution_id.trim().is_empty() {
            return Err("execution identifier cannot be empty".into());
        }
        if record.owner_session.as_deref() != Some(self.installation_id.as_str()) {
            return Err("process is not owned by this Medusa installation".into());
        }
        let process_id = record.id.as_str().to_owned();
        self.state.registry.register(record).map_err(|e| e.to_string())?;
        self.state.supervisors.insert(execution_id.clone(), SupervisorState::new(&execution_id).map_err(str::to_owned)?);
        self.state.process_executions.insert(process_id.clone(), execution_id.clone());
        self.state.wakeups.subscribe(WakeupSubscription {
            id: SubscriptionId::parse(format!("recover-{execution_id}")).map_err(str::to_owned)?,
            owner: execution_id.clone(),
            source: WakeupSource::ProcessOrphaned(process_id.clone()),
            one_shot: true,
            enabled: true,
        }).map_err(str::to_owned)?;
        self.state.events.push(SupervisionEvent::ProcessRegistered { process_id, execution_id });
        self.persist()
    }

    pub fn heartbeat(&mut self, process_id: &ProcessId, execution_id: &str, checkpoint: Option<String>, now: OffsetDateTime) -> Result<(), String> {
        self.ensure_binding(process_id, execution_id)?;
        self.state.registry.get_mut(process_id).map_err(|e| e.to_string())?.heartbeat(now).map_err(|e| e.to_string())?;
        if let Some(checkpoint) = checkpoint {
            if checkpoint.len() != 64 || !checkpoint.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("checkpoint fingerprint must be a SHA-256 digest".into());
            }
        }
        self.state.events.push(SupervisionEvent::HeartbeatRecorded {
            process_id: process_id.as_str().to_owned(), execution_id: execution_id.to_owned(),
        });
        self.persist()
    }

    pub fn record_shutdown(&mut self, process_id: &ProcessId, execution_id: &str, clean: bool, now: OffsetDateTime) -> Result<(), String> {
        self.ensure_binding(process_id, execution_id)?;
        let record = self.state.registry.get_mut(process_id).map_err(|e| e.to_string())?;
        let target = if clean { ProcessState::Exited } else { ProcessState::Failed };
        record.transition(target, now).map_err(|e| e.to_string())?;
        self.state.events.push(SupervisionEvent::RuntimeShutdown {
            process_id: process_id.as_str().to_owned(), execution_id: execution_id.to_owned(), clean,
        });
        self.persist()
    }

    pub fn reconcile(&mut self, now: OffsetDateTime, timeout: Duration, is_alive: impl Fn(u32) -> bool) -> Result<Vec<WakeupDelivery>, String> {
        let orphaned = self.state.registry.reconcile(now, timeout, is_alive);
        let mut deliveries = Vec::new();
        for process_id in orphaned {
            let Some(execution_id) = self.state.process_executions.get(process_id.as_str()).cloned() else { continue; };
            let owned = self.state.registry.get(&process_id).and_then(|record| record.owner_session.as_deref()) == Some(self.installation_id.as_str());
            if !owned {
                continue;
            }
            if let Some(supervisor) = self.state.supervisors.get_mut(&execution_id) {
                if supervisor.resumable() {
                    supervisor.apply(Signal::RecoveryRequired { reason: "process missing or heartbeat stale".into() }).map_err(str::to_owned)?;
                }
            }
            self.state.events.push(SupervisionEvent::ProcessOrphaned {
                process_id: process_id.as_str().to_owned(), execution_id: execution_id.clone(), reason: "process missing or heartbeat stale".into(),
            });
            let event = WakeupEvent {
                sequence: self.state.next_wakeup_sequence,
                occurred_at: now,
                source: WakeupSource::ProcessOrphaned(process_id.as_str().to_owned()),
                metadata: BTreeMap::from([("execution_id".into(), execution_id)]),
            };
            self.state.next_wakeup_sequence = self.state.next_wakeup_sequence.saturating_add(1);
            deliveries.extend(self.state.wakeups.route(event).map_err(str::to_owned)?);
        }
        self.persist()?;
        Ok(deliveries)
    }

    pub fn recover_once(&mut self, candidate: &RecoveryCandidate) -> Result<Option<RecoveryDecision>, String> {
        if !self.state.recovered_executions.insert(candidate.execution_id.clone()) {
            return Ok(None);
        }
        let lock = self.recovery.acquire_lock(&candidate.transaction_id, &self.installation_id, 1).map_err(|e| e.to_string())?;
        let decision = self.recovery.decide(candidate, &lock).map_err(|e| e.to_string())?;
        RecoveryCoordinator::verify_decision(&decision).map_err(|e| e.to_string())?;
        self.state.events.push(SupervisionEvent::RecoverySelected {
            execution_id: decision.execution_id.clone(), action: decision.action.clone(), reason: decision.reason.clone(),
        });
        if decision.action == RecoveryAction::Quarantine {
            self.state.events.push(SupervisionEvent::TerminalActionRequired {
                execution_id: decision.execution_id.clone(), reason: decision.reason.clone(),
            });
        }
        self.persist()?;
        Ok(Some(decision))
    }

    pub fn events(&self) -> &[SupervisionEvent] { &self.state.events }
    pub fn registry(&self) -> &ProcessRegistry { &self.state.registry }

    fn ensure_binding(&self, process_id: &ProcessId, execution_id: &str) -> Result<(), String> {
        if self.state.process_executions.get(process_id.as_str()).map(String::as_str) != Some(execution_id) {
            return Err("process and execution identifiers are not registered together".into());
        }
        let record = self.state.registry.get(process_id).ok_or_else(|| "process is not registered".to_owned())?;
        if record.owner_session.as_deref() != Some(self.installation_id.as_str()) {
            return Err("process is not owned by this Medusa installation".into());
        }
        Ok(())
    }

    fn persist(&self) -> Result<(), String> {
        let parent = self.path.parent().ok_or_else(|| "control-plane path has no parent".to_owned())?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let temp = self.path.with_extension("json.tmp");
        fs::write(&temp, serde_json::to_vec_pretty(&self.state).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        fs::rename(temp, &self.path).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use medusa_process_registry::{ProcessSpec};
    use medusa_recovery_coordinator::TransactionPhase;
    use time::macros::datetime;

    fn digest(value: u8) -> String { format!("{value:02x}").repeat(32) }
    fn record(id: &str, owner: &str) -> ProcessRecord {
        let mut record = ProcessRecord::new(ProcessId::parse(id).unwrap(), ProcessSpec {
            program: "medusa-runtime".into(), args: vec![], working_directory: None, restartable: true,
        }, datetime!(2026-07-26 08:00 UTC), Some(owner.into())).unwrap();
        record.mark_running(42, datetime!(2026-07-26 08:00 UTC)).unwrap();
        record
    }
    fn candidate() -> RecoveryCandidate {
        RecoveryCandidate { transaction_id: "tx-1".into(), execution_id: "exec-1".into(), phase: TransactionPhase::Committing,
            checkpoint_sequence: 2, checkpoint_fingerprint: digest(1), snapshot_fingerprint: digest(2), replay_fingerprint: digest(3), rollback_fingerprint: Some(digest(4)) }
    }

    #[test]
    fn restart_reloads_registry_and_pending_wakeup_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("resilience.json");
        let mut plane = ResilienceControlPlane::load_or_create(&path, "install-a").unwrap();
        plane.register(record("runtime-1", "install-a"), "exec-1").unwrap();
        drop(plane);
        let loaded = ResilienceControlPlane::load_or_create(&path, "install-a").unwrap();
        assert!(loaded.registry().get(&ProcessId::parse("runtime-1").unwrap()).is_some());
    }

    #[test]
    fn orphaning_wakes_once_and_recovery_is_deduplicated() {
        let dir = tempfile::tempdir().unwrap();
        let mut plane = ResilienceControlPlane::load_or_create(dir.path().join("state.json"), "install-a").unwrap();
        plane.register(record("runtime-1", "install-a"), "exec-1").unwrap();
        assert_eq!(plane.reconcile(datetime!(2026-07-26 08:10 UTC), Duration::minutes(5), |_| false).unwrap().len(), 1);
        assert!(plane.recover_once(&candidate()).unwrap().is_some());
        assert!(plane.recover_once(&candidate()).unwrap().is_none());
    }

    #[test]
    fn foreign_process_is_never_adopted_or_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let mut plane = ResilienceControlPlane::load_or_create(dir.path().join("state.json"), "install-a").unwrap();
        assert!(plane.register(record("foreign", "install-b"), "exec-x").is_err());
    }

    #[test]
    fn terminal_decision_surfaces_user_action() {
        let dir = tempfile::tempdir().unwrap();
        let mut plane = ResilienceControlPlane::load_or_create(dir.path().join("state.json"), "install-a").unwrap();
        let mut item = candidate();
        item.phase = TransactionPhase::Failed;
        let decision = plane.recover_once(&item).unwrap().unwrap();
        assert_eq!(decision.action, RecoveryAction::Quarantine);
        assert!(plane.events().iter().any(|event| matches!(event, SupervisionEvent::TerminalActionRequired { .. })));
    }
}
