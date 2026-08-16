from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    target = Path(path)
    text = target.read_text()
    if old not in text:
        raise SystemExit(f"missing anchor in {target}: {old[:120]!r}")
    target.write_text(text.replace(old, new, 1))


agent = Path("crates/medusa-agent/src/delegation.rs")
text = agent.read_text()
old = """pub struct DelegationAttemptBinding {
    pub schema_version: u16,
    pub fingerprint: String,
    pub contract_id: String,
    pub contract_fingerprint: String,
    pub lease_epoch: u64,
    pub attempt_ordinal: u32,
    pub session_id: String,
    pub predecessor_session_id: Option<String>,
}"""
new = """pub struct DelegationAttemptBinding {
    pub schema_version: u16,
    pub fingerprint: String,
    pub contract_id: String,
    pub contract_fingerprint: String,
    pub lease_epoch: u64,
    pub attempt_ordinal: u32,
    pub session_id: String,
    pub predecessor_session_id: Option<String>,
    pub effective_allowed_tools: Vec<String>,
    pub unavailable_tools: Vec<String>,
}"""
if old not in text:
    raise SystemExit("attempt struct anchor missing")
text = text.replace(old, new, 1)

start = text.index("impl DelegationAttemptBinding {")
end = text.index(
    "\n#[derive(Clone, Debug, Eq, PartialEq)]\npub struct DelegationCapabilitySnapshot",
    start,
)
replacement = """impl DelegationAttemptBinding {
    pub fn new(
        contract: &DelegationContract,
        lease_epoch: u64,
        attempt_ordinal: u32,
        session_id: String,
        predecessor_session_id: Option<String>,
        mut effective_allowed_tools: Vec<String>,
    ) -> Result<Self, String> {
        if lease_epoch == 0 || attempt_ordinal == 0 || session_id.trim().is_empty() {
            return Err("delegation attempt binding is incomplete".to_owned());
        }
        contract.validate()?;
        effective_allowed_tools.sort();
        effective_allowed_tools.dedup();
        if effective_allowed_tools
            .iter()
            .any(|tool| contract.authority.allowed_tools.binary_search(tool).is_err())
        {
            return Err("delegation attempt cannot add tools outside its contract".to_owned());
        }
        let unavailable_tools = contract
            .authority
            .allowed_tools
            .iter()
            .filter(|tool| effective_allowed_tools.binary_search(tool).is_err())
            .cloned()
            .collect::<Vec<_>>();
        let fingerprint = sha256(
            &serde_json::to_vec(&(
                DELEGATION_CONTRACT_SCHEMA_VERSION,
                &contract.contract_id,
                &contract.fingerprint,
                lease_epoch,
                attempt_ordinal,
                &session_id,
                &predecessor_session_id,
                &effective_allowed_tools,
                &unavailable_tools,
            ))
            .map_err(|error| error.to_string())?,
        );
        Ok(Self {
            schema_version: DELEGATION_CONTRACT_SCHEMA_VERSION,
            fingerprint,
            contract_id: contract.contract_id.clone(),
            contract_fingerprint: contract.fingerprint.clone(),
            lease_epoch,
            attempt_ordinal,
            session_id,
            predecessor_session_id,
            effective_allowed_tools,
            unavailable_tools,
        })
    }

    pub fn validate(&self, contract: &DelegationContract) -> Result<(), String> {
        if self.schema_version != DELEGATION_CONTRACT_SCHEMA_VERSION
            || self.contract_id != contract.contract_id
            || self.contract_fingerprint != contract.fingerprint
            || self.lease_epoch == 0
            || self.attempt_ordinal == 0
            || self.session_id.trim().is_empty()
        {
            return Err("delegation attempt does not match its contract".to_owned());
        }
        let expected = Self::new(
            contract,
            self.lease_epoch,
            self.attempt_ordinal,
            self.session_id.clone(),
            self.predecessor_session_id.clone(),
            self.effective_allowed_tools.clone(),
        )?;
        if expected != *self {
            return Err("delegation attempt fingerprint or narrowing mismatch".to_owned());
        }
        Ok(())
    }
}
"""
text = text[:start] + replacement + text[end:]

old = """pub fn delegation_execution_policy(
    contract: &DelegationContract,
    role: TeamRole,
) -> AgentExecutionPolicy {
    AgentExecutionPolicy::for_team_role(role)
        .intersect_allowed_tools(contract.authority.allowed_tools.clone())
        .with_allowed_write_paths(contract.authority.write_scopes.clone())
        .with_delegation_binding(contract.contract_id.clone(), contract.fingerprint.clone())
}"""
new = """pub fn delegation_execution_policy(
    contract: &DelegationContract,
    role: TeamRole,
    effective_allowed_tools: impl IntoIterator<Item = String>,
) -> AgentExecutionPolicy {
    AgentExecutionPolicy::for_team_role(role)
        .intersect_allowed_tools(contract.authority.allowed_tools.clone())
        .intersect_allowed_tools(effective_allowed_tools)
        .with_allowed_write_paths(contract.authority.write_scopes.clone())
        .with_delegation_binding(contract.contract_id.clone(), contract.fingerprint.clone())
}"""
if old not in text:
    raise SystemExit("execution policy anchor missing")
text = text.replace(old, new, 1)

old = '                "predecessor_session_id": attempt.predecessor_session_id,\n            }),'
new = '                "predecessor_session_id": attempt.predecessor_session_id,\n                "effective_allowed_tools": attempt.effective_allowed_tools,\n                "unavailable_tools": attempt.unavailable_tools,\n            }),' 
if old not in text:
    raise SystemExit("session binding evidence anchor missing")
text = text.replace(old, new, 1)

insert = r'''

    #[test]
    fn attempt_cannot_inherit_new_global_tools() {
        let contract = DelegationContract::seal(None, material()).expect("contract");
        let attempt = DelegationAttemptBinding::new(
            &contract,
            1,
            1,
            "session-a".into(),
            None,
            vec!["fs_read".into()],
        )
        .expect("attempt");
        assert_eq!(attempt.effective_allowed_tools, vec!["fs_read"]);
        assert!(
            DelegationAttemptBinding::new(
                &contract,
                1,
                2,
                "session-b".into(),
                Some("session-a".into()),
                vec!["fs_read".into(), "fs_write".into()],
            )
            .is_err()
        );
    }

    #[test]
    fn retry_attempt_records_capability_loss_and_session_lineage() {
        let mut authority = material();
        authority.allowed_tools = vec!["fs_read".into(), "web_fetch".into()];
        let contract = DelegationContract::seal(None, authority).expect("contract");
        let first = DelegationAttemptBinding::new(
            &contract,
            1,
            1,
            "session-a".into(),
            None,
            contract.authority.allowed_tools.clone(),
        )
        .expect("first");
        let second = DelegationAttemptBinding::new(
            &contract,
            2,
            2,
            "session-b".into(),
            Some(first.session_id.clone()),
            vec!["fs_read".into()],
        )
        .expect("second");
        assert_eq!(second.contract_id, first.contract_id);
        assert_eq!(second.predecessor_session_id.as_deref(), Some("session-a"));
        assert_eq!(second.unavailable_tools, vec!["web_fetch"]);
    }

    #[test]
    fn delegated_write_scope_remains_bounded_after_round_trip() {
        let mut authority = material();
        authority.mode = Mode::Yolo;
        authority.write_scopes = vec!["src/lib.rs".into()];
        authority.mutation_authority = DelegatedMutationAuthority::IsolatedWorktree;
        authority.allowed_tools = vec!["fs_read".into(), "fs_write".into()];
        let contract = DelegationContract::seal(None, authority).expect("contract");
        let encoded = serde_json::to_vec(&contract).expect("serialize");
        let restored: DelegationContract = serde_json::from_slice(&encoded).expect("restore");
        let policy = delegation_execution_policy(
            &restored,
            TeamRole::Implementer,
            restored.authority.allowed_tools.clone(),
        );
        assert!(
            policy
                .denial_reason("fs_write", &json!({"path":"src/lib.rs"}))
                .is_none()
        );
        assert!(
            policy
                .denial_reason("fs_write", &json!({"path":"src/sibling.rs"}))
                .is_some()
        );
    }

    #[test]
    fn provider_route_is_pinned_in_contract() {
        let mut authority = material();
        authority.model.provider = "pinned-provider".into();
        authority.model.name = "pinned-model".into();
        let contract = DelegationContract::seal(None, authority).expect("contract");
        let mut ambient = ModelConfig::default();
        ambient.provider = "new-default".into();
        ambient.name = "new-model".into();
        assert_eq!(contract.authority.model.provider, "pinned-provider");
        assert_eq!(contract.authority.model.name, "pinned-model");
        assert_ne!(contract.authority.model, ambient);
    }

    #[test]
    fn successor_contract_explicitly_links_predecessor() {
        let base = DelegationContract::seal(None, material()).expect("base");
        let mut authority = material();
        authority.allowed_tools.push("web_fetch".into());
        let successor = DelegationContract::seal(Some(base.contract_id.clone()), authority)
            .expect("successor");
        assert_eq!(
            successor.predecessor_contract_id.as_deref(),
            Some(base.contract_id.as_str())
        );
        assert_ne!(successor.contract_id, base.contract_id);
    }

    #[test]
    fn corrupt_or_missing_contract_fails_closed() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = DelegationContractStore::new(directory.path());
        assert!(store.load_contract("delegation-v1-missing").is_err());
        let contract = DelegationContract::seal(None, material()).expect("contract");
        let path = store.persist_contract(&contract).expect("persist");
        fs::write(&path, b"{not-json").expect("corrupt");
        assert!(store.load_contract(&contract.contract_id).is_err());
    }
'''
module_end = text.rfind("\n}")
if module_end < 0:
    raise SystemExit("delegation test module end missing")
text = text[:module_end] + insert + text[module_end:]
agent.write_text(text)

runtime = Path("crates/medusa-runtime/src/delegation_contract.rs")
text = runtime.read_text()
old = """    let existing = controller.delegation_contract_binding(request.task_id);

    let contract = if let Some(binding) = existing.as_ref() {"""
new = """    let existing = controller.delegation_contract_binding(request.task_id);
    validate_contract_presence(existing.as_ref(), request.task_id, request.lease_epoch)?;
    let current_capabilities =
        snapshot_delegated_capabilities(request.capability_repo, request.mode, &base_policy)?;

    let contract = if let Some(binding) = existing.as_ref() {"""
if old not in text:
    raise SystemExit("runtime existing contract anchor missing")
text = text.replace(old, new, 1)
old = """        let capabilities =
            snapshot_delegated_capabilities(request.capability_repo, request.mode, &base_policy)?;
        let allowed_tools = capabilities.allowed_tools;"""
if old not in text:
    raise SystemExit("new contract capability anchor missing")
text = text.replace(old, "        let allowed_tools = current_capabilities.allowed_tools.clone();", 1)
text = text.replace(
    "capability_registry_schema_version: capabilities.schema_version,",
    "capability_registry_schema_version: current_capabilities.schema_version,",
    1,
)
text = text.replace(
    "capability_registry_fingerprint: capabilities.fingerprint,",
    "capability_registry_fingerprint: current_capabilities.fingerprint.clone(),",
    1,
)
old = """    let previous = store.latest_attempt(&contract.contract_id)?;
    let attempt_ordinal = previous
        .as_ref()
        .map_or(1, |attempt| attempt.attempt_ordinal.saturating_add(1));
    let attempt = DelegationAttemptBinding::new(
        &contract,
        request.lease_epoch,
        attempt_ordinal,
        request.session_id.to_string(),
        previous.map(|attempt| attempt.session_id),
    )?;"""
new = """    let effective_allowed_tools = contract
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
    )?;"""
if old not in text:
    raise SystemExit("attempt creation anchor missing")
text = text.replace(old, new, 1)
old = """pub(crate) fn policy_for(contract: &DelegationContract, role: TeamRole) -> AgentExecutionPolicy {
    delegation_execution_policy(contract, role)
}"""
new = """pub(crate) fn policy_for(
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
}"""
if old not in text:
    raise SystemExit("policy helper anchor missing")
text = text.replace(old, new, 1)
old = """    let authority = &contract.authority;
    let expected = json!({"""
new = """    let authority = &contract.authority;
    let request_policy_fingerprint = fingerprint_json(
        &AgentExecutionPolicy::for_team_role(request.role)
            .with_allowed_write_paths(request.write_scopes.clone())
            .audit_projection(),
    )?;
    let expected = json!({"""
if old not in text:
    raise SystemExit("validate existing policy anchor missing")
text = text.replace(old, new, 1)
text = text.replace(
    '        "role_mode": request.mode,\n    });',
    '        "role_mode": request.mode,\n        "root_execution_id": request.root_execution_id,\n        "objective": request.objective,\n        "required_evidence": request.required_evidence,\n        "max_delegation_depth": request.max_delegation_depth,\n    });',
    1,
)
text = text.replace(
    '        "role_mode": authority.mode,\n    });',
    '        "role_mode": authority.mode,\n        "root_execution_id": authority.root_execution_id,\n        "objective": authority.objective,\n        "required_evidence": authority.required_evidence,\n        "max_delegation_depth": authority.max_delegation_depth,\n    });',
    1,
)
old = """        || binding.worker_id != request.worker_id
        || binding.accepted_lease_epoch != authority.accepted_lease_epoch"""
new = """        || binding.worker_id != request.worker_id
        || binding.accepted_lease_epoch != authority.accepted_lease_epoch
        || request_policy_fingerprint != authority.role_policy_fingerprint"""
if old not in text:
    raise SystemExit("validation condition anchor missing")
text = text.replace(old, new, 1)
text += r'''

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_retry_without_contract_is_rejected() {
        assert!(validate_contract_presence(None, "implement", 1).is_ok());
        let error = validate_contract_presence(None, "implement", 2).expect_err("legacy retry");
        assert!(error.contains("legacy_contract_unknown"));
    }
}
'''
runtime.write_text(text)

for file, old, new in [
    (
        "crates/medusa-runtime/src/multi_agent_coordinator.rs",
        "policy_for(&request.delegation, role)",
        "policy_for(&request.delegation, &request.attempt, role)",
    ),
    (
        "crates/medusa-runtime/src/mutating_worker_coordinator.rs",
        "policy_for(&request.delegation, TeamRole::Implementer)",
        "policy_for(&request.delegation, &request.attempt, TeamRole::Implementer)",
    ),
]:
    replace_once(file, old, new)
