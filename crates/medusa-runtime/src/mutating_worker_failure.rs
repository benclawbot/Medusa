//! Durable failure recording and cleanup for mutating worker attempts.

use std::{path::Path, sync::mpsc::Sender};

use medusa_agent::{LeasedAssignment, TeamRuntime, WorkerExecutionController};
use medusa_workers::{Worker, WorkerManager, WorkerState};

use super::support::write_atomic;
use super::{DurableImplementationState, ImplementationStatus, WorkerRun};
use crate::{RuntimeEvent, team_control::TeamControlPlane};

#[allow(clippy::too_many_arguments)]
pub(super) fn record_attempt_failure(
    controller: &mut WorkerExecutionController,
    team: &TeamRuntime,
    events: &Sender<RuntimeEvent>,
    control: &TeamControlPlane,
    manager: &WorkerManager,
    state_path: &Path,
    assignment: &LeasedAssignment,
    worker: &Worker,
    mut state: DurableImplementationState,
    run: Option<&WorkerRun>,
    changed_paths: Vec<String>,
    verification_evidence: Vec<String>,
    error: String,
    retryable: bool,
    cancelled: bool,
) -> Result<String, String> {
    let mut secondary = Vec::new();
    let controller_result = if cancelled {
        controller.cancel(
            &assignment.task_id,
            &assignment.worker_id,
            assignment.lease_epoch,
        )
    } else {
        controller.fail(
            &assignment.task_id,
            &assignment.worker_id,
            assignment.lease_epoch,
            &error,
            retryable,
        )
    };
    if let Err(controller_error) = controller_result {
        secondary.push(format!(
            "durable task failure recording failed: {controller_error}"
        ));
    }
    if let Err(team_error) = team.finish_member(&assignment.worker_id, true) {
        secondary.push(format!(
            "team lifecycle failure recording failed: {team_error}"
        ));
    }
    let preserve_corrective_worktree = retryable
        && !cancelled
        && error.starts_with("isolated worktree verification failed:")
        && !verification_evidence.is_empty();
    if !preserve_corrective_worktree
        && let Err(cleanup_error) = manager.cleanup(std::slice::from_ref(worker))
    {
        secondary.push(format!("isolated resource cleanup failed: {cleanup_error}"));
    }
    if let Some(run) = run {
        state.session_id.clone_from(&run.session_id);
        state.turns = run.turns;
        state.summary.clone_from(&run.summary);
    }
    state.worker.state = if preserve_corrective_worktree {
        WorkerState::Ready
    } else {
        WorkerState::Failed
    };
    state.changed_paths = changed_paths;
    state.verification_evidence = verification_evidence;
    state.status = if retryable && secondary.is_empty() {
        ImplementationStatus::Retrying
    } else {
        ImplementationStatus::Failed
    };
    let recorded = if secondary.is_empty() {
        error
    } else {
        format!("{error}; {}", secondary.join("; "))
    };
    state.last_error = Some(recorded.clone());
    write_atomic(state_path, &state)?;
    let snapshot = if state.status == ImplementationStatus::Retrying {
        control.retrying(&assignment.worker_id, recorded.clone())
    } else {
        control.fail(&assignment.worker_id, recorded.clone())
    };
    if let Ok(snapshot) = snapshot {
        let _ = events.send(RuntimeEvent::Team(snapshot));
    }
    if secondary.is_empty() {
        Ok(recorded)
    } else {
        Err(recorded)
    }
}
