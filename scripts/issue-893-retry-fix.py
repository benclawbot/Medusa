from pathlib import Path

path = Path("crates/medusa-runtime/src/delegation_contract.rs")
text = path.read_text()

old = '''    let authority = &contract.authority;
    let expected = json!({
        "task_id": request.task_id,
        "worker_id": request.worker_id,
        "plan_fingerprint": request.plan_fingerprint,
        "context_fingerprint": request.context_fingerprint,
        "repository_identity": request.repository_identity,
        "repository_revision": request.repository_revision,
        "write_scopes": request.write_scopes,
        "mutation_authority": request.mutation_authority,
        "role_mode": request.mode,
        "root_execution_id": request.root_execution_id,
        "objective": request.objective,
        "required_evidence": request.required_evidence,
        "max_delegation_depth": request.max_delegation_depth,
    });'''
new = '''    let authority = &contract.authority;
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
    });'''
if old not in text:
    raise SystemExit("retry reconciliation projection anchor missing")
text = text.replace(old, new, 1)

anchor = '''fn validate_existing(
    contract: &DelegationContract,'''
helper = '''fn canonical_strings(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

'''
if anchor not in text:
    raise SystemExit("validate_existing function anchor missing")
text = text.replace(anchor, helper + anchor, 1)

old = '''    fn legacy_retry_without_contract_is_rejected() {
        assert!(validate_contract_presence(None, "implement", 1).is_ok());
        let error = validate_contract_presence(None, "implement", 2).expect_err("legacy retry");
        assert!(error.contains("legacy_contract_unknown"));
    }
}'''
new = '''    fn legacy_retry_without_contract_is_rejected() {
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
}'''
if old not in text:
    raise SystemExit("delegation contract test module anchor missing")
path.write_text(text.replace(old, new, 1))
