from pathlib import Path


def replace_once(source: str, old: str, new: str, label: str) -> str:
    if new in source:
        return source
    count = source.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one target, found {count}")
    return source.replace(old, new, 1)


runtime_path = Path("crates/medusa-runtime/src/lib.rs")
runtime = runtime_path.read_text()
runtime = replace_once(
    runtime,
    "pub mod attachment;\npub mod checkpoint_store;\n",
    "pub mod attachment;\npub mod checkpoint_payload;\npub mod checkpoint_store;\n",
    "checkpoint payload module",
)
runtime = replace_once(
    runtime,
    "pub use checkpoint_store::RuntimeCheckpointRecord;\n",
    "pub use checkpoint_payload::{CheckpointFilePayload, RuntimeCheckpointPayload};\npub use checkpoint_store::RuntimeCheckpointRecord;\n",
    "checkpoint payload exports",
)
runtime = replace_once(
    runtime,
    '''    pub fn execute_recovery(
        &self,
        view: medusa_recovery_coordinator::RecoveryView,
        request: medusa_recovery_coordinator::RecoveryActionRequest,
        preflight: medusa_recovery_coordinator::RecoveryPreflightEvidence,
    ) -> Result<(), RuntimeError> {
        if lock_submission(&self.submission).busy {
            return Err(RuntimeError::Busy);
        }
        self.commands
            .send(RuntimeCommand::Recovery {
                view: Box::new(view),
                request,
                preflight,
            })
            .map_err(|_| RuntimeError::WorkerStopped)
    }
''',
    '''    pub fn execute_recovery(
        &self,
        view: medusa_recovery_coordinator::RecoveryView,
        request: medusa_recovery_coordinator::RecoveryActionRequest,
        preflight: medusa_recovery_coordinator::RecoveryPreflightEvidence,
    ) -> Result<(), RuntimeError> {
        if lock_submission(&self.submission).busy {
            return Err(RuntimeError::Busy);
        }
        let (view, preflight) = if matches!(
            request.operation,
            medusa_recovery_coordinator::RecoveryOperation::RestoreCheckpoint
        ) {
            let checkpoint_id = request
                .checkpoint_id
                .as_deref()
                .ok_or_else(|| RuntimeError::InvalidCommand("restore requires a checkpoint id".to_owned()))?;
            self.preview_checkpoint_restore(&request.session_id, checkpoint_id)?;
            let (authoritative_view, authoritative_preflight) =
                recovery_action_context(&self.repo, &request).map_err(RuntimeError::agent)?;
            if preflight != authoritative_preflight {
                return Err(RuntimeError::InvalidCommand(
                    "recovery preflight is stale; refresh the checkpoint preview".to_owned(),
                ));
            }
            let source_cursor = self.execution_health(&request.session_id)?.journal_cursor;
            record_controller_event(
                &self.repo,
                &request.session_id,
                Actor::User,
                EventPayload::CheckpointRestoreRequested {
                    checkpoint_id: checkpoint_id.to_owned(),
                    source_cursor,
                },
            )?;
            (authoritative_view, authoritative_preflight)
        } else {
            (view, preflight)
        };
        self.commands
            .send(RuntimeCommand::Recovery {
                view: Box::new(view),
                request,
                preflight,
            })
            .map_err(|_| RuntimeError::WorkerStopped)
    }
''',
    "restore request lineage",
)
runtime_path.write_text(runtime)

protocol_path = Path("crates/medusa-protocol/src/lib.rs")
protocol = protocol_path.read_text()
protocol = replace_once(
    protocol,
    '''    RecoveryActionCompleted {
        receipt: Value,
    },
    CancellationRequested {
''',
    '''    RecoveryActionCompleted {
        receipt: Value,
    },
    CheckpointRestoreRequested {
        checkpoint_id: String,
        source_cursor: u64,
    },
    CancellationRequested {
''',
    "restore protocol event",
)
protocol_path.write_text(protocol)

history_path = Path("crates/medusa-runtime/src/execution_history.rs")
history = history_path.read_text()
history = replace_once(
    history,
    "    let log = execution_log(&session_id, &journal_events)?;\n",
    "    let log = execution_log(repo, &session_id, &journal_events)?;\n",
    "execution log repository",
)
history = replace_once(
    history,
    '''fn execution_log(session_id: &str, events: &[EventEnvelope]) -> Result<ExecutionLog, RuntimeError> {
''',
    '''fn execution_log(
    repo: &Path,
    session_id: &str,
    events: &[EventEnvelope],
) -> Result<ExecutionLog, RuntimeError> {
''',
    "execution log signature",
)
history = replace_once(
    history,
    "        repository_receipt_fingerprint(events)?,\n",
    "        crate::checkpoint_payload::repository_fingerprint(repo, events)?,\n",
    "repository snapshot fingerprint",
)
history = replace_once(
    history,
    '''            EventPayload::RecoveryActionCompleted { receipt } => {
                values.insert("recovery".to_owned(), digest_lossy(receipt));
            }
            EventPayload::VerificationCompleted { passed, evidence } => {
''',
    '''            EventPayload::RecoveryActionCompleted { receipt } => {
                values.insert("recovery".to_owned(), digest_lossy(receipt));
            }
            EventPayload::CheckpointRestoreRequested {
                checkpoint_id,
                source_cursor,
            } => {
                values.insert("restore_checkpoint".to_owned(), checkpoint_id.clone());
                values.insert("restore_source_cursor".to_owned(), source_cursor.to_string());
            }
            EventPayload::VerificationCompleted { passed, evidence } => {
''',
    "restore state reduction",
)
history = replace_once(
    history,
    '        EventPayload::RecoveryActionCompleted { .. } => "recovery_action_completed",\n',
    '        EventPayload::RecoveryActionCompleted { .. } => "recovery_action_completed",\n        EventPayload::CheckpointRestoreRequested { .. } => "checkpoint_restore_requested",\n',
    "restore payload kind",
)
start = history.find("fn repository_receipt_fingerprint(")
if start != -1:
    end = history.find("fn subsystem_fingerprints(", start)
    if end == -1:
        raise SystemExit("repository receipt helper end not found")
    history = history[:start] + history[end:]
history_path.write_text(history)

checkpoint_path = Path("crates/medusa-runtime/src/checkpoint_store.rs")
checkpoint = checkpoint_path.read_text()
checkpoint = replace_once(
    checkpoint,
    '''    persist(repo, &record)?;
    Ok(record)
''',
    '''    persist(repo, &record)?;
    crate::checkpoint_payload::materialize(repo, &record)?;
    Ok(record)
''',
    "checkpoint payload materialization",
)
checkpoint_path.write_text(checkpoint)

recovery_path = Path("crates/medusa-runtime/src/recovery_tui.rs")
recovery = recovery_path.read_text()
recovery = replace_once(
    recovery,
    '''use std::{
    collections::BTreeMap,
    fs, io,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
''',
    '''use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
''',
    "recovery imports",
)
recovery = recovery.replace("use serde::Deserialize;\nuse thiserror::Error;\n", "use serde::Deserialize;\n\nuse crate::{RuntimeError, checkpoint_payload};\n")
start = recovery.find('const CHECKPOINT_PAYLOAD_DIRECTORY: &str = ".medusa/recovery-checkpoints";')
if start != -1:
    end = recovery.find("struct RuntimeRecoveryExecutor", start)
    if end == -1:
        raise SystemExit("recovery payload declarations end not found")
    recovery = recovery[:start] + recovery[end:]
recovery = recovery.replace("    type Error = RuntimeRecoveryError;\n", "    type Error = RuntimeError;\n")
recovery = replace_once(
    recovery,
    '''                restore_checkpoint(repository, &action.session_id, checkpoint_id)?;
''',
    '''                let payload =
                    checkpoint_payload::load(repository, &action.session_id, checkpoint_id)?;
                checkpoint_payload::restore(repository, &payload)?;
''',
    "transactional restore call",
)
start = recovery.find("fn restore_checkpoint(")
if start != -1:
    end = recovery.find("fn now_unix_ms()", start)
    if end == -1:
        raise SystemExit("legacy restore helper end not found")
    recovery = recovery[:start] + recovery[end:]
start = recovery.find("#[cfg(test)]\nmod tests {")
if start != -1:
    recovery = recovery[:start]
recovery_path.write_text(recovery)
