use medusa_runtime::component_runtime::{
    AgentRuntimeFacade, ComponentId, ComponentRuntime, ComponentSpec, DesiredStateMutation,
    DesiredStateProposal, DesiredStateStore, ProposalError, ProposalSource,
};

#[test]
fn proposal_preview_reports_impact_and_commit_applies_only_via_reconciler() {
    let store = DesiredStateStore::new();
    let mut runtime = ComponentRuntime::new();
    let facade = AgentRuntimeFacade::new(&store);
    let proposal = DesiredStateProposal::new(
        "proposal-1",
        0,
        vec![DesiredStateMutation::upsert(ComponentSpec::new("worker"))],
        ProposalSource::new("agent-1", "task-7"),
    )
    .with_idempotency_key("proposal-1");

    let preview = facade.preview(&proposal, &runtime).expect("preview");
    assert_eq!(
        preview.affected_components,
        vec![ComponentId::new("worker").expect("id")]
    );
    assert!(preview.predicted_restarts.is_empty());

    let commit = facade.commit(&proposal, &runtime).expect("commit");
    assert_eq!(commit.receipt.revision, 1);
    assert_eq!(commit.audit.source.agent_id, "agent-1");
    assert!(runtime.active_generations("worker").is_empty());
    facade
        .apply(&mut runtime, &commit)
        .expect("reconcile apply");
    assert_eq!(runtime.active_generations("worker").len(), 1);
    assert_eq!(store.audit_records(), vec![commit.audit]);
}

#[test]
fn stale_proposal_cannot_mutate_desired_or_actual_runtime_state() {
    let store = DesiredStateStore::new();
    let runtime = ComponentRuntime::new();
    let facade = AgentRuntimeFacade::new(&store);
    let first = DesiredStateProposal::new(
        "first",
        0,
        vec![DesiredStateMutation::upsert(ComponentSpec::new("first"))],
        ProposalSource::new("agent-1", "task-1"),
    );
    facade.commit(&first, &runtime).expect("first commit");
    let stale = DesiredStateProposal::new(
        "stale",
        0,
        vec![DesiredStateMutation::upsert(ComponentSpec::new("stale"))],
        ProposalSource::new("agent-2", "task-2"),
    );
    let error = facade.commit(&stale, &runtime).expect_err("stale conflict");
    assert!(matches!(error, ProposalError::DesiredState(_)));
    assert_eq!(store.snapshot().revision, 1);
    assert!(runtime.active_generations("stale").is_empty());
}

#[test]
fn invalid_proposal_is_rejected_before_cas_and_keeps_audits_clean() {
    let store = DesiredStateStore::new();
    let runtime = ComponentRuntime::new();
    let facade = AgentRuntimeFacade::new(&store);
    let proposal = DesiredStateProposal::new(
        "invalid",
        0,
        vec![DesiredStateMutation::upsert(
            ComponentSpec::new("consumer").with_requirement(
                medusa_runtime::component_runtime::DependencyRequirement::required("missing"),
            ),
        )],
        ProposalSource::new("agent-1", "task-3"),
    );
    let error = facade
        .commit(&proposal, &runtime)
        .expect_err("invalid proposal");
    assert!(matches!(error, ProposalError::DesiredState(_)));
    assert_eq!(store.snapshot().revision, 0);
    assert!(store.audit_records().is_empty());
}

#[test]
fn forged_commit_receipt_cannot_bypass_the_audited_proposal_path() {
    let store = DesiredStateStore::new();
    let mut runtime = ComponentRuntime::new();
    let facade = AgentRuntimeFacade::new(&store);
    let proposal = DesiredStateProposal::new(
        "proposal-1",
        0,
        vec![DesiredStateMutation::upsert(ComponentSpec::new("worker"))],
        ProposalSource::new("agent-1", "task-1"),
    );
    let mut forged = facade.commit(&proposal, &runtime).expect("commit");
    forged.audit.proposal_id = "forged".to_owned();

    let error = facade
        .apply(&mut runtime, &forged)
        .expect_err("forged audit");
    assert!(matches!(error, ProposalError::DirectMutationDenied));
    assert!(runtime.active_generations("worker").is_empty());
}
