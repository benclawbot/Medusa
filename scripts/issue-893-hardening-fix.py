from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    target = Path(path)
    text = target.read_text()
    if old not in text:
        raise SystemExit(f"missing anchor in {target}: {old[:140]!r}")
    target.write_text(text.replace(old, new, 1))


# Retry admission: the original role-policy fingerprint is immutable provenance, not an
# equality requirement. Current policy may narrow authority; it may never widen past the
# persisted contract.
runtime = Path("crates/medusa-runtime/src/delegation_contract.rs")
text = runtime.read_text()
old = """    let authority = &contract.authority;
    let request_policy_fingerprint = fingerprint_json(
        &AgentExecutionPolicy::for_team_role(request.role)
            .with_allowed_write_paths(request.write_scopes.clone())
            .audit_projection(),
    )?;
    let expected = json!({"""
new = """    let authority = &contract.authority;
    let expected = json!({"""
if old not in text:
    raise SystemExit("retry role-policy equality block missing")
text = text.replace(old, new, 1)
old = """        || binding.worker_id != request.worker_id
        || binding.accepted_lease_epoch != authority.accepted_lease_epoch
        || request_policy_fingerprint != authority.role_policy_fingerprint"""
new = """        || binding.worker_id != request.worker_id
        || binding.accepted_lease_epoch != authority.accepted_lease_epoch"""
if old not in text:
    raise SystemExit("retry role-policy equality condition missing")
text = text.replace(old, new, 1)
runtime.write_text(text)

# Read-only evidence carries the exact contract and attempt identities used for the model
# session. Deterministic fast-lane evidence is intentionally not model-backed and therefore
# leaves these fields empty.
coordinator = Path("crates/medusa-runtime/src/multi_agent_coordinator.rs")
text = coordinator.read_text()
old = """    WorkerExecutionController, bind_session_to_delegation,
};"""
new = """    DelegationContractStore, WorkerExecutionController, bind_session_to_delegation,
};"""
if old not in text:
    raise SystemExit("coordinator delegation import anchor missing")
text = text.replace(old, new, 1)
old = """    #[serde(default)]
    pub lease_epoch: u64,
    pub session_id: String,"""
new = """    #[serde(default)]
    pub lease_epoch: u64,
    #[serde(default)]
    pub delegation_contract_id: String,
    #[serde(default)]
    pub delegation_contract_fingerprint: String,
    #[serde(default)]
    pub delegation_attempt_fingerprint: String,
    pub session_id: String,"""
if old not in text:
    raise SystemExit("worker evidence struct anchor missing")
text = text.replace(old, new, 1)
old = """                    \"## {} ({:?}, session {})\\n{}\",
                    worker.task_id, worker.role, worker.session_id, worker.summary"""
new = """                    \"## {} ({:?}, session {}, delegation {})\\n{}\",
                    worker.task_id,
                    worker.role,
                    worker.session_id,
                    if worker.delegation_contract_id.is_empty() {
                        \"deterministic\"
                    } else {
                        worker.delegation_contract_id.as_str()
                    },
                    worker.summary"""
if old not in text:
    raise SystemExit("parent context evidence rendering anchor missing")
text = text.replace(old, new, 1)
old = """            lease_epoch: 1,
            session_id: format!("""
new = """            lease_epoch: 1,
            delegation_contract_id: String::new(),
            delegation_contract_fingerprint: String::new(),
            delegation_attempt_fingerprint: String::new(),
            session_id: format!("""
if old not in text:
    raise SystemExit("deterministic evidence constructor anchor missing")
text = text.replace(old, new, 1)
old = """        context_fingerprint: request.packet.fingerprint,
        lease_epoch: 0,
        session_id: session.id.to_string(),"""
new = """        context_fingerprint: request.packet.fingerprint,
        lease_epoch: 0,
        delegation_contract_id: request.delegation.contract_id,
        delegation_contract_fingerprint: request.delegation.fingerprint,
        delegation_attempt_fingerprint: request.attempt.fingerprint,
        session_id: session.id.to_string(),"""
if old not in text:
    raise SystemExit("production worker evidence constructor anchor missing")
text = text.replace(old, new, 1)
old = """    let evidence_directory = root.join(\"worker-evidence\");
    let mut evidence = load_worker_evidence(&evidence_directory)?;
    for worker in &evidence {
        validate_worker_evidence(plan, &contract_by_task, worker)?;"""
new = """    let evidence_directory = root.join(\"worker-evidence\");
    let delegation_store = DelegationContractStore::new(&root);
    let mut evidence = load_worker_evidence(&evidence_directory)?;
    for worker in &evidence {
        validate_worker_evidence(plan, &contract_by_task, worker)?;
        validate_worker_delegation_evidence(&delegation_store, &controller, worker)?;"""
if old not in text:
    raise SystemExit("persisted worker evidence recovery anchor missing")
text = text.replace(old, new, 1)
old = """        result.lease_epoch = assignment.lease_epoch;
        write_atomic("""
new = """        result.lease_epoch = assignment.lease_epoch;
        if let Err(error) =
            validate_worker_delegation_evidence(&delegation_store, &controller, &result)
        {
            controller.fail(&task_id, &worker_id, assignment.lease_epoch, &error, true)?;
            team.finish_member(&worker_id, true)?;
            failures.push(format!(\"{task_id} ({worker_id}): {error}\"));
            continue;
        }
        write_atomic("""
if old not in text:
    raise SystemExit("new worker evidence validation anchor missing")
text = text.replace(old, new, 1)
insert_before = """fn validate_worker_evidence(
    plan: &ProductionExecutionPlan,"""
helper = r'''fn validate_worker_delegation_evidence(
    store: &DelegationContractStore,
    controller: &WorkerExecutionController,
    evidence: &WorkerEvidence,
) -> Result<(), String> {
    if evidence.delegation_contract_id.trim().is_empty()
        || evidence.delegation_contract_fingerprint.trim().is_empty()
        || evidence.delegation_attempt_fingerprint.trim().is_empty()
    {
        return Err(format!(
            "legacy_contract_unknown: worker evidence for {} has no delegation identity",
            evidence.task_id
        ));
    }
    let binding = controller
        .delegation_contract_binding(&evidence.task_id)
        .ok_or_else(|| {
            format!(
                "legacy_contract_unknown: worker evidence for {} has no scheduler contract binding",
                evidence.task_id
            )
        })?;
    if binding.contract_id != evidence.delegation_contract_id
        || binding.contract_fingerprint != evidence.delegation_contract_fingerprint
        || binding.worker_id != evidence.worker_id
    {
        return Err(format!(
            "delegation evidence for {} does not match durable scheduler authority",
            evidence.task_id
        ));
    }
    let contract = store.load_contract(&binding.contract_id)?;
    if contract.fingerprint != evidence.delegation_contract_fingerprint {
        return Err(format!(
            "delegation evidence for {} does not match the persisted contract",
            evidence.task_id
        ));
    }
    let attempt = store
        .latest_attempt(&binding.contract_id)?
        .ok_or_else(|| format!("delegation attempt is missing for {}", evidence.task_id))?;
    attempt.validate(&contract)?;
    if attempt.fingerprint != evidence.delegation_attempt_fingerprint
        || attempt.session_id != evidence.session_id
        || attempt.lease_epoch != evidence.lease_epoch
    {
        return Err(format!(
            "delegation evidence for {} does not match its persisted attempt",
            evidence.task_id
        ));
    }
    Ok(())
}

'''
if insert_before not in text:
    raise SystemExit("worker evidence helper insertion anchor missing")
text = text.replace(insert_before, helper + insert_before, 1)
coordinator.write_text(text)

# Mutating implementation state and parent-review evidence retain the same immutable authority
# identity. Legacy prepared/running states without it fail closed instead of being resumed.
mutating = Path("crates/medusa-runtime/src/mutating_worker_coordinator.rs")
text = mutating.read_text()
old = """    context_fingerprint: String,
    session_id: String,"""
new = """    context_fingerprint: String,
    #[serde(default)]
    delegation_contract_id: String,
    #[serde(default)]
    delegation_contract_fingerprint: String,
    #[serde(default)]
    delegation_attempt_fingerprint: String,
    session_id: String,"""
if old not in text:
    raise SystemExit("durable implementation state identity anchor missing")
text = text.replace(old, new, 1)
old = """    pub worker_id: String,
    pub session_id: String,"""
new = """    pub worker_id: String,
    pub delegation_contract_id: String,
    pub delegation_contract_fingerprint: String,
    pub delegation_attempt_fingerprint: String,
    pub session_id: String,"""
if old not in text:
    raise SystemExit("implementation evidence identity anchor missing")
text = text.replace(old, new, 1)
old = """        let team_context = team.member_context(&assignment.worker_id)?;
        team.start_member(&assignment.worker_id, &assignment.task_id, \"starting\")?;"""
new = """        let delegation_contract_id = resolved.contract.contract_id.clone();
        let delegation_contract_fingerprint = resolved.contract.fingerprint.clone();
        let delegation_attempt_fingerprint = resolved.attempt.fingerprint.clone();
        let team_context = team.member_context(&assignment.worker_id)?;
        team.start_member(&assignment.worker_id, &assignment.task_id, \"starting\")?;"""
if old not in text:
    raise SystemExit("resolved mutating delegation identity anchor missing")
text = text.replace(old, new, 1)
old = """            context_fingerprint: packet.fingerprint.clone(),
            session_id: String::new(),"""
new = """            context_fingerprint: packet.fingerprint.clone(),
            delegation_contract_id,
            delegation_contract_fingerprint,
            delegation_attempt_fingerprint,
            session_id: String::new(),"""
if old not in text:
    raise SystemExit("running implementation state identity anchor missing")
text = text.replace(old, new, 1)
old = """            context_fingerprint: packet.fingerprint.clone(),
            session_id: run.session_id,"""
new = """            context_fingerprint: packet.fingerprint.clone(),
            delegation_contract_id: running.delegation_contract_id.clone(),
            delegation_contract_fingerprint: running.delegation_contract_fingerprint.clone(),
            delegation_attempt_fingerprint: running.delegation_attempt_fingerprint.clone(),
            session_id: run.session_id,"""
if old not in text:
    raise SystemExit("prepared implementation state identity anchor missing")
text = text.replace(old, new, 1)
# Speculative assumption WorkerEvidence is deterministic/non-model evidence.
old = """            lease_epoch: 1,
            session_id: format!(\"speculative-assumption-{dependency}\"),"""
new = """            lease_epoch: 1,
            delegation_contract_id: String::new(),
            delegation_contract_fingerprint: String::new(),
            delegation_attempt_fingerprint: String::new(),
            session_id: format!(\"speculative-assumption-{dependency}\"),"""
if old not in text:
    raise SystemExit("speculative assumption evidence identity anchor missing")
text = text.replace(old, new, 1)
mutating.write_text(text)

support = Path("crates/medusa-runtime/src/mutating_worker_coordinator_support.rs")
text = support.read_text()
old = """    if matches!(
        state.status,
        ImplementationStatus::Prepared | ImplementationStatus::Integrated
    ) && (state.worker.commit.is_none()"""
new = """    if state.delegation_contract_id.trim().is_empty()
        || state.delegation_contract_fingerprint.trim().is_empty()
        || state.delegation_attempt_fingerprint.trim().is_empty()
    {
        return Err(
            \"legacy_contract_unknown: durable implementation state has no complete delegation identity\"
                .to_owned(),
        );
    }
    if matches!(
        state.status,
        ImplementationStatus::Prepared | ImplementationStatus::Integrated
    ) && (state.worker.commit.is_none()"""
if old not in text:
    raise SystemExit("legacy mutating state fail-closed anchor missing")
text = text.replace(old, new, 1)
old = """        worker_id: state.worker.id.clone(),
        session_id: state.session_id.clone(),"""
new = """        worker_id: state.worker.id.clone(),
        delegation_contract_id: state.delegation_contract_id.clone(),
        delegation_contract_fingerprint: state.delegation_contract_fingerprint.clone(),
        delegation_attempt_fingerprint: state.delegation_attempt_fingerprint.clone(),
        session_id: state.session_id.clone(),"""
if old not in text:
    raise SystemExit("implementation evidence projection anchor missing")
text = text.replace(old, new, 1)
support.write_text(text)
