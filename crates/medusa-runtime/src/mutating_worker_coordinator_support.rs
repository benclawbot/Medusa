//! Validation and durable-state helpers for mutating worktree coordination.

use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use medusa_multi_agent_scheduler::Task;
use serde::Serialize;

use crate::{
    multi_agent_coordinator::CoordinatorEvidence,
    mutation_transaction::MutationTransaction,
    production_orchestrator::{AgentContract, AgentRole, ContextPacket, ProductionExecutionPlan},
};

use super::{
    DurableImplementationState, IMPLEMENTER_ID, ImplementationEvidence, ImplementationStatus,
};

pub(super) fn implementation_contract(
    plan: &ProductionExecutionPlan,
) -> Result<AgentContract, String> {
    let mut contracts = plan
        .contracts
        .iter()
        .filter(|contract| contract.role == AgentRole::Implementer)
        .cloned()
        .collect::<Vec<_>>();
    contracts.sort_by(|left, right| left.task_id.cmp(&right.task_id));
    match contracts.as_slice() {
        [contract] => Ok(contract.clone()),
        [] => Err("coordinated plan has no implementer contract".to_owned()),
        _ => Err("this integration slice requires exactly one implementer contract".to_owned()),
    }
}

pub(super) fn implementation_worker_label(
    plan: &ProductionExecutionPlan,
    contract: &AgentContract,
) -> String {
    let suffix = plan.fingerprint.chars().take(12).collect::<String>();
    format!("{}-{suffix}", contract.task_id)
}

pub(super) fn implementation_task(
    plan: &ProductionExecutionPlan,
    contract: &AgentContract,
) -> Result<Task, String> {
    plan.tasks
        .iter()
        .find(|task| task.id == contract.task_id)
        .cloned()
        .ok_or_else(|| format!("implementation task {} is missing", contract.task_id))
}

pub(super) fn dependency_outputs(
    contract: &AgentContract,
    preflight: &CoordinatorEvidence,
) -> Result<BTreeMap<String, String>, String> {
    let outputs = preflight
        .workers
        .iter()
        .filter(|worker| contract.dependencies.contains(&worker.task_id))
        .map(|worker| (worker.task_id.clone(), worker.summary.clone()))
        .collect::<BTreeMap<_, _>>();
    if contract
        .dependencies
        .iter()
        .any(|dependency| !outputs.contains_key(dependency))
    {
        return Err("implementer context is missing durable dependency evidence".to_owned());
    }
    Ok(outputs)
}

pub(super) fn validate_changed_paths(
    contract: &AgentContract,
    paths: &[String],
) -> Result<(), String> {
    if paths.is_empty() {
        return Err("mutating worker completed without repository changes".to_owned());
    }
    let allowed = contract
        .allowed_write_paths
        .iter()
        .map(|path| normalize(path))
        .collect::<Vec<_>>();
    for path in paths {
        let normalized = normalize(path);
        if normalized.starts_with(".git/") || normalized == ".git" {
            return Err("worker attempted to include Git metadata in its commit".to_owned());
        }
        if !allowed.iter().any(|scope| scope_allows(scope, &normalized)) {
            return Err(format!(
                "worker changed out-of-scope path `{normalized}`; allowed scopes are {allowed:?}"
            ));
        }
    }
    Ok(())
}

fn normalize(path: &str) -> String {
    path.replace('\\', "/")
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_owned()
}

fn scope_allows(scope: &str, path: &str) -> bool {
    matches!(scope, "" | "." | "repository")
        || path == scope
        || path
            .strip_prefix(scope)
            .is_some_and(|remainder| remainder.starts_with('/'))
}

pub(super) fn validate_preflight(
    plan: &ProductionExecutionPlan,
    preflight: &CoordinatorEvidence,
) -> Result<(), String> {
    if preflight.plan_fingerprint != plan.fingerprint
        || preflight.repository_fingerprint.trim().is_empty()
        || preflight.workers.len() < 2
    {
        return Err(
            "implementation preflight evidence does not match the execution plan".to_owned(),
        );
    }
    Ok(())
}

pub(super) fn validate_state(
    plan: &ProductionExecutionPlan,
    preflight: &CoordinatorEvidence,
    packet: &ContextPacket,
    state: &DurableImplementationState,
) -> Result<(), String> {
    if state.plan_fingerprint != plan.fingerprint
        || state.repository_fingerprint != preflight.repository_fingerprint
        || state.context_fingerprint != packet.fingerprint
        || state.worker.id != IMPLEMENTER_ID
        || state.base_head.trim().is_empty()
        || state.lease_epoch == 0
    {
        return Err("durable implementation state does not match the current execution".to_owned());
    }
    if matches!(
        state.status,
        ImplementationStatus::Prepared | ImplementationStatus::Integrated
    ) && (state.worker.commit.is_none()
        || state.session_id.trim().is_empty()
        || state.summary.trim().is_empty()
        || state.changed_paths.is_empty()
        || state.changed_components.is_empty()
        || state.verification_evidence.is_empty()
        || state.verification_receipt.is_none())
    {
        return Err("durable prepared implementation evidence is incomplete".to_owned());
    }
    Ok(())
}

pub(super) fn evidence_from_state(
    state_path: &Path,
    task_id: &str,
    state: &DurableImplementationState,
) -> Result<ImplementationEvidence, String> {
    let transaction_path = if state.transaction_path.as_os_str().is_empty() {
        state_path
            .parent()
            .ok_or_else(|| "implementation state path has no execution root".to_owned())?
            .join("mutation-transaction.json")
    } else {
        state.transaction_path.clone()
    };
    let transaction = MutationTransaction::open(&transaction_path)?;
    let snapshot = transaction.snapshot();
    Ok(ImplementationEvidence {
        plan_fingerprint: state.plan_fingerprint.clone(),
        repository_fingerprint: state.repository_fingerprint.clone(),
        task_id: task_id.to_owned(),
        worker_id: state.worker.id.clone(),
        session_id: state.session_id.clone(),
        turns: state.turns,
        summary: state.summary.clone(),
        changed_paths: state.changed_paths.clone(),
        changed_components: state.changed_components.clone(),
        verification_evidence: state.verification_evidence.clone(),
        verification_receipt: state
            .verification_receipt
            .clone()
            .ok_or_else(|| "prepared implementation has no typed verification receipt".to_owned())?,
        base_head: snapshot.base_head.clone(),
        prepared_commit: snapshot.prepared_commit.clone(),
        prepared_tree: snapshot.prepared_tree.clone(),
        patch_fingerprint: snapshot.patch_fingerprint.clone(),
        review_context: transaction.review_context()?,
        transaction_path,
        state_path: state_path.to_path_buf(),
    })
}

pub(super) fn load_state(path: &Path) -> Result<DurableImplementationState, String> {
    serde_json::from_slice(&fs::read(path).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
}

pub(super) fn write_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "durable state path has no parent".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("json.tmp");
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    fs::rename(temporary, path).map_err(|error| error.to_string())
}

pub(super) fn now_ms() -> Result<u64, String> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_millis();
    u64::try_from(millis).map_err(|_| "system time exceeded u64 milliseconds".to_owned())
}
