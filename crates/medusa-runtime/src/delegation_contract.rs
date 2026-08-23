//! Runtime admission of immutable delegation contracts.

use std::path::Path;

use medusa_agent::{
    AgentExecutionPolicy, DelegatedApprovalPolicy, DelegatedMutationAuthority,
    DelegationAttemptBinding, DelegationContract, DelegationContractMaterial,
    DelegationContractStore, DelegationLeaseBinding, TeamRole, WorkerExecutionController,
    delegation_execution_policy, fingerprint_json, snapshot_delegated_capabilities,
    DELEGATION_ROLE_POLICY_VERSION, DELEGATION_SYSTEM_POLICY_VERSION,
};
use medusa_config::{Config, Mode};
use medusa_core::SessionId;
use serde_json::json;

pub(crate) struct DelegationRequest<'a> {
    pub capability_repo: &'a Path,
    pub execution_root: &'a Path,
    pub root_execution_id: &'a str,
    pub plan_fingerprint: &'a str,
    pub context_fingerprint: &'a str,
    pub objective: &'a str,
    pub required_evidence: &'a [String],
    pub worker_id: &'a str,
    pub task_id: &'a str,
    pub lease_epoch: u64,
    pub session_id: &'a SessionId,
    pub config: &'a Config,
    pub role: TeamRole,
    pub mode: Mode,
    pub max_turns: u32,
    pub max_attempts: u32,
    pub max_delegation_depth: u8,
    pub repository_identity: &'a str,
    pub repository_revision: &'a str,
    pub worktree_identity: Option<String>,
    pub write_scopes: Vec<String>,
    pub mutation_authority: DelegatedMutationAuthority,
}

pub(crate) struct ResolvedDelegation {
    pub contract: DelegationContract,
    pub attempt: DelegationAttemptBinding,
}

pub(crate) fn resolve_delegation(
    controller: &mut WorkerExecutionController,
    request: DelegationRequest<'_>,
) -> Result<ResolvedDelegation, String> {
    let store = DelegationContractStore::new(request.execution_root);
    let base_policy = AgentExecutionPolicy::for_team_role(request.role)
        .with_allowed_write_paths(request.write_scopes.clone());
    let existing = controller.delegation_contract_binding(request.task_id);
    validate_contract_presence(existing.as_ref(), request.task_id, request.lease_epoch)?;
    let current_capabilities =
        snapshot_delegated_capabilities(request.capability_repo, request.mode, &base_policy)?;

    let contract = if let Some(binding) = existing.as_ref() {
        let contract = store.load_contract(&binding.contract_id)?;
        validate_existing(&contract, &request, binding)?;
        contract
    } else {
        let allowed_tools = current_capabilities.allowed_tools.clone();
        let role_policy_fingerprint = fingerprint_json(&base_policy.audit_projection())?;
        let network_allowed = allowed_tools
            .iter()
            .any(|tool| matches!(tool.as_str(), "web_search" | "web_fetch"));
        let process_allowed = allowed_tools.iter().any(|tool| tool == "shell_run");
        let browser_allowed = allowed_tools
            .iter()
            .any(|tool| tool.starts_with("browser_"));
        let authority = DelegationContractMaterial {
            root_execution_id: request.root_execution_id.to_owned(),
            parent_worker_id: "lead".to_owned(),
            parent_session_id: None,
            worker_id: request.worker_id.to_owned(),
            task_id: request.task_id.to_owned(),
            accepted_lease_epoch: request.lease_epoch,
            delegation_depth: 0,
            max_delegation_depth: request.max_delegation_depth,
            initial_session_id: request.session_id.to_string(),
            repository_identity: request.repository_identity.to_owned(),
            repository_revision: request.repository_revision.to_owned(),
            worktree_identity: request.worktree_identity.clone(),
            read_scopes: vec!["repository".to_owned()],
            write_scopes: request.write_scopes.clone(),
            mutation_authority: request.mutation_authority,
            capability_registry_schema_version: current_capabilities.schema_version,
            capability_registry_fingerprint: current_capabilities.fingerprint.clone(),
            allowed_tools,
            role_policy_version: DELEGATION_ROLE_POLICY_VERSION,
            role_policy_fingerprint,
            allow_user_questions: false,
            approval_policy: DelegatedApprovalPolicy::Never,
            network_allowed,
            process_allowed,
            browser_allowed,
            credentialed_actions_allowed: false,
            model: request.config.model.clone(),
            mode: request.mode,
            max_turns: request.max_turns.max(1),
            max_model_calls: request.max_turns.max(1),
            retry_attempts: request.max_attempts.max(1),
            context_fingerprint: request.context_fingerprint.to_owned(),
            plan_fingerprint: request.plan_fingerprint.to_owned(),
            objective: request.objective.to_owned(),
            required_evidence: request.required_evidence.to_vec(),
            system_policy_version: DELEGATION_SYSTEM_POLICY_VERSION.to_owned(),
        };
        let contract = DelegationContract::seal(None, authority)?;
        store.persist_contract(&contract)?;
        contract
    };

    let binding = existing.unwrap_or_else(|| DelegationLeaseBinding {
        contract_id: contract.contract_id.clone(),
        contract_fingerprint: contract.fingerprint.clone(),
        worker_id: request.worker_id.to_owned(),
        accepted_lease_epoch: request.lease_epoch,
    });
    controller.bind_delegation_contract(
        request.task_id,
        request.worker_id,
        request.lease_epoch,
        binding,
    )?;

    let effective_allowed_tools = contract
        .authority
        .allowed_tools
        .iter()
        .filter(|tool| current_capabilities.allowed_tools.binary_search(tool).is_ok())
        .cloned()
        .collect::<Vec<_>>();
    let previous = store.latest_attempt(&contract.contract_id)?;
    let attempt_ordinal = previous
        .as_ref()
        .map_or(1, |attempt| attempt.attempt_ordinal.saturating_add(1));
    let attempt = DelegationAttemptBinding::new(
        &contract,
        request.lease_epoch,
        attempt_ordinal,
        request.session_id.to_string(),
        previous.map(|attempt| attempt.session_id),
        effective_allowed_tools,
    )?;
    store.persist_attempt(&attempt)?;

    Ok(ResolvedDelegation { contract, attempt })
}

pub(crate) fn policy_for(
    contract: &DelegationContract,
    attempt: &DelegationAttemptBinding,
    role: TeamRole,
) -> AgentExecutionPolicy {
    delegation_execution_policy(contract, role, attempt.effective_allowed_tools.clone())
}

fn validate_contract_presence(
    binding: Option<&DelegationLeaseBinding>,
    task_id: &str,
    lease_epoch: u64,
) -> Result<(), String> {
    if binding.is_none() && lease_epoch > 1 {
        return Err(format!(
            "legacy_contract_unknown: task {task_id} reached lease epoch {lease_epoch} without a durable delegation contract"
        ));
    }
    Ok(())
}

fn canonical_strings(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

fn validate_existing(
    contract: &DelegationContract,
    request: &DelegationRequest<'_>,
    binding: &DelegationLeaseBinding,
) -> Result<(), String> {
    contract.validate()?;
    let authority = &contract.authority;
    let expected = json!({
        "task_id": request.task_id,
        "worker_id": request.worker_id,
        "plan_fingerprint": request.plan_fingerprint,
        "context_fingerprint": request.context_fingerprint,
        "repository_identity": request.repository_identity,
        "repository_revision": request.repository_revision,
        "write_scopes": canonical_strings(request.write_scopes.clone()),
        "mutation_authority": request.mutation_authority,
        "role_mode": request.mode,
        "root_execution_id": request.root_execution_id,
        "objective": request.objective,
        "required_evidence": canonical_strings(request.required_evidence.to_vec()),
        "max_delegation_depth": request.max_delegation_depth,
    });
    let recorded = json!({
        "task_id": authority.task_id,
        "worker_id": authority.worker_id,
        "plan_fingerprint": authority.plan_fingerprint,
        "context_fingerprint": authority.context_fingerprint,
        "repository_identity": authority.repository_identity,
        "repository_revision": authority.repository_revision,
        "write_scopes": authority.write_scopes,
        "mutation_authority": authority.mutation_authority,
        "role_mode": authority.mode,
        "root_execution_id": authority.root_execution_id,
        "objective": authority.objective,
        "required_evidence": authority.required_evidence,
        "max_delegation_depth": authority.max_delegation_depth,
    });
    if expected != recorded
        || authority.model != request.config.model
        || binding.contract_fingerprint != contract.fingerprint
        || binding.worker_id != request.worker_id
        || binding.accepted_lease_epoch != authority.accepted_lease_epoch
    {
        return Err(format!(
            "delegation_reconciliation_required: durable contract {} does not match current task authority",
            contract.contract_id
        ));
    }
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_retry_without_contract_is_rejected() {
        assert!(validate_contract_presence(None, "implement", 1).is_ok());
        let error = validate_contract_presence(None, "implement", 2).expect_err("legacy retry");
        assert!(error.contains("legacy_contract_unknown"));
    }

    #[test]
    fn retry_authority_sets_are_canonicalized_before_reconciliation() {
        assert_eq!(
            canonical_strings(vec![
                "patch or commit evidence".to_owned(),
                "focused tests".to_owned(),
                "focused tests".to_owned(),
            ]),
            vec!["focused tests".to_owned(), "patch or commit evidence".to_owned()]
        );
    }
}
