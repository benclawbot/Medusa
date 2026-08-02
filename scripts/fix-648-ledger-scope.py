from pathlib import Path


def replace_once(path: Path, before: str, after: str, label: str) -> None:
    text = path.read_text(encoding="utf-8")
    if text.count(before) != 1:
        raise RuntimeError(f"{label} did not match exactly once")
    path.write_text(text.replace(before, after, 1), encoding="utf-8")


orchestrator = Path("crates/medusa-runtime/src/production_orchestrator.rs")
replace_once(
    orchestrator,
    '''pub fn open_ledger(repo: &Path, plan: &ProductionExecutionPlan) -> Result<ExecutionLedger, String> {
    let path = repo
        .join(".medusa")
        .join("executions")
        .join(&plan.fingerprint)
        .join("execution-ledger.json");
    let mut ledger = ExecutionLedger::open_or_create(path, &plan.planning)?;
    ledger.recover_interrupted()?;
    Ok(ledger)
}
''',
    '''pub fn open_ledger(
    repo: &Path,
    session_id: &str,
    plan: &ProductionExecutionPlan,
) -> Result<ExecutionLedger, String> {
    if session_id.trim().is_empty() {
        return Err("durable execution ledger requires a session identity".to_owned());
    }
    let execution_key = digest(&(session_id, &plan.fingerprint));
    let path = repo
        .join(".medusa")
        .join("executions")
        .join(execution_key)
        .join("execution-ledger.json");
    let mut ledger = ExecutionLedger::open_or_create(path, &plan.planning)?;
    ledger.recover_interrupted()?;
    Ok(ledger)
}
''',
    "session-scoped ledger function",
)
replace_once(
    orchestrator,
    "        let mut ledger = open_ledger(directory.path(), &planned).unwrap();\n",
    "        let mut ledger = open_ledger(directory.path(), \"test-session\", &planned).unwrap();\n",
    "existing ledger test call",
)
marker = '''    #[test]
    fn durable_projection_comes_from_ledger() {
'''
isolation_test = '''    #[test]
    fn identical_plans_do_not_share_state_across_sessions() {
        let directory = tempfile::tempdir().unwrap();
        let draft = PromptDraft {
            text: "Analyze repository architecture without changing files".to_owned(),
            ..PromptDraft::default()
        };
        let planned = plan_for_repository(directory.path(), &draft).unwrap();
        let mut first = open_ledger(directory.path(), "session-a", &planned).unwrap();
        first.begin("analyze", "worker-a").unwrap();
        let second = open_ledger(directory.path(), "session-b", &planned).unwrap();
        assert_ne!(first.path(), second.path());
        assert!(matches!(
            first
                .views()
                .into_iter()
                .find(|view| view.id == "analyze")
                .map(|view| view.state),
            Some(LedgerTaskState::Running { .. })
        ));
        assert!(matches!(
            second
                .views()
                .into_iter()
                .find(|view| view.id == "analyze")
                .map(|view| view.state),
            Some(LedgerTaskState::Pending { attempts: 0 })
        ));
    }

'''
replace_once(
    orchestrator,
    marker,
    isolation_test + marker,
    "session isolation regression test",
)

runtime = Path("crates/medusa-runtime/src/lib.rs")
replace_once(
    runtime,
    '''        let ledger = crate::production_orchestrator::open_ledger(&state.repo, &execution_plan)
            .map_err(RuntimeError::agent)?;
''',
    '''        let ledger = crate::production_orchestrator::open_ledger(
            &state.repo,
            session.id.as_str(),
            &execution_plan,
        )
        .map_err(RuntimeError::agent)?;
''',
    "runtime session ledger binding",
)
