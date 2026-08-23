//! Durable failure recording and cleanup for mutating worker attempts.

use std::{path::Path, sync::mpsc::Sender};

use medusa_agent::{LeasedAssignment, TeamRuntime, WorkerExecutionController};
use medusa_execution_checkpoint::{RetryHypothesis, StepCapsule};
use medusa_workers::{Worker, WorkerManager, WorkerState};
use sha2::{Digest, Sha256};

use super::support::write_atomic;
use super::{DurableImplementationState, ImplementationStatus, WorkerRun};
use crate::{RuntimeEvent, team_control::TeamControlPlane};

fn digest(value: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(value.as_ref()))
}

fn retry_capsule(
    state: &DurableImplementationState,
    assignment: &LeasedAssignment,
    error: &str,
) -> Result<StepCapsule, String> {
    let authority_fingerprint = if state.delegation_contract_fingerprint.trim().is_empty() {
        digest(format!(
            "{}:{}:{}",
            state.plan_fingerprint, state.context_fingerprint, assignment.task_id
        ))
    } else {
        state.delegation_contract_fingerprint.clone()
    };
    let capsule_root = format!(
        "{}:{}:{}:{}",
        state.plan_fingerprint,
        assignment.task_id,
        assignment.worker_id,
        assignment.lease_epoch
    );
    let objective = format!(
        "Retry delegated mutating task `{}` from durable failure evidence without reusing the failed reasoning context",
        assignment.task_id
    );
    let acceptance_criteria = vec![
        "remain inside the delegated mutation authority".to_owned(),
        "change the failed strategy before executing the retry".to_owned(),
        "produce independently verifiable evidence before promotion".to_owned(),
    ];
    let previous = StepCapsule::new(
        format!("{capsule_root}:failed-attempt"),
        state.plan_fingerprint.clone(),
        state.plan_fingerprint.clone(),
        1,
        assignment.task_id.clone(),
        objective.clone(),
        acceptance_criteria.clone(),
        state.changed_paths.clone(),
        Vec::new(),
        state.changed_paths.clone(),
        Vec::new(),
        None,
        None,
        authority_fingerprint.clone(),
    )
    .map_err(|error| error.to_string())?;
    let failure_fingerprint = digest(error);
    let hypothesis = RetryHypothesis {
        failure_fingerprint: failure_fingerprint.clone(),
        previous_hypothesis: None,
        disproving_evidence: vec![failure_fingerprint.clone()],
        new_hypothesis: format!(
            "the recorded failure for `{}` requires rebuilding the bounded context before choosing the next action",
            assignment.task_id
        ),
        changed_strategy: "start from the durable delegation contract and failure evidence; do not continue the prior reasoning trace".to_owned(),
        environment_fingerprint: digest(format!(
            "{}:{}:{}",
            state.base_head, state.context_fingerprint, assignment.lease_epoch
        )),
    };
    let retry = StepCapsule::new(
        format!("{capsule_root}:fresh-retry"),
        state.plan_fingerprint.clone(),
        state.plan_fingerprint.clone(),
        1,
        assignment.task_id.clone(),
        objective,
        acceptance_criteria,
        state.changed_paths.clone(),
        Vec::new(),
        state.changed_paths.clone(),
        Vec::new(),
        Some(failure_fingerprint),
        Some(hypothesis),
        authority_fingerprint,
    )
    .map_err(|error| error.to_string())?;
    retry
        .verify_retry_from(&previous)
        .map_err(|error| error.to_string())?;
    Ok(retry)
}

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
        && error.starts_with("isolated worktree verification failed:");
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
    let mut recorded = if secondary.is_empty() {
        error
    } else {
        format!("{error}; {}", secondary.join("; "))
    };
    if state.status == ImplementationStatus::Retrying {
        let capsule = retry_capsule(&state, assignment, &recorded)?;
        let capsule_json = serde_json::to_string(&capsule).map_err(|error| error.to_string())?;
        recorded.push_str("\nFresh retry Step Capsule (authoritative): ");
        recorded.push_str(&capsule_json);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use medusa_workers::{WorkerKind, WorkerState};
    use std::path::PathBuf;

    fn worker() -> Worker {
        Worker {
            id: "worker-implement".to_owned(),
            kind: WorkerKind::Worktree,
            worktree: PathBuf::from("/tmp/worker"),
            branch: "medusa/worker".to_owned(),
            base_head: digest("base"),
            commit: None,
            state: WorkerState::Ready,
        }
    }

    fn state() -> DurableImplementationState {
        DurableImplementationState {
            plan_fingerprint: digest("plan"),
            repository_fingerprint: digest("repository"),
            base_head: digest("head"),
            lease_epoch: 1,
            status: ImplementationStatus::Running,
            worker: worker(),
            context_fingerprint: digest("context"),
            delegation_contract_id: "delegation-1".to_owned(),
            delegation_contract_fingerprint: digest("delegation"),
            delegation_attempt_fingerprint: digest("attempt"),
            session_id: "session-1".to_owned(),
            turns: 1,
            summary: String::new(),
            changed_paths: vec!["src/lib.rs".to_owned()],
            changed_components: Vec::new(),
            verification_evidence: Vec::new(),
            verification_receipt: None,
            transaction_path: PathBuf::new(),
            last_error: None,
            speculative: false,
            speculation_ledger_path: PathBuf::new(),
            speculation_assumptions_fingerprint: String::new(),
            speculation_branch: String::new(),
        }
    }

    #[test]
    fn retry_capsule_is_fresh_and_bound_to_failure() {
        let assignment = LeasedAssignment {
            task_id: "implementation".to_owned(),
            worker_id: "worker-implement".to_owned(),
            lease_epoch: 1,
        };
        let capsule = retry_capsule(&state(), &assignment, "verification failed")
            .expect("retry capsule");
        capsule.verify().expect("verified capsule");
        assert!(capsule.fresh_context);
        assert!(capsule.retry_hypothesis.is_some());
        assert_eq!(capsule.failure_fingerprint, Some(digest("verification failed")));
    }
}
