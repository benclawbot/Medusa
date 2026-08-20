use medusa_multi_agent_scheduler::{
    ExecutionStrategy, PlannerInput, PlanningIntent, TaskKind, plan_typed,
};

#[test]
fn protected_files_do_not_downgrade_an_explicit_repair_to_read_only() {
    let objective = "Inspect this repository and repair all three product defects without modifying verify.py, test.mjs, package.json, fixtures, or expected outputs. Correct value.txt to the verified value, robustly implement src/slugify.py while preserving its public API, and repair the counter transitions in src/counter.js.";
    let planned = plan_typed(PlannerInput {
        objective: objective.to_owned(),
        attachment_count: 0,
        repository_paths: vec![
            "package.json".to_owned(),
            "src/counter.js".to_owned(),
            "src/slugify.py".to_owned(),
            "test.mjs".to_owned(),
            "value.txt".to_owned(),
            "verify.py".to_owned(),
        ],
    })
    .expect("mixed mutation objective should plan");

    assert_eq!(planned.intent, PlanningIntent::MutationRequested);
    assert_eq!(planned.strategy, ExecutionStrategy::CoordinatedMutation);
    assert!(planned.task(TaskKind::Implementation).is_some());
    assert_eq!(planned.requested_outcomes, vec![objective.to_owned()]);
    planned
        .validate()
        .expect("restored objective must remain fingerprint-valid");
}

#[test]
fn live_repair_objective_resolves_sentence_terminated_counter_path() {
    let objective = "Inspect this repository and repair all three product defects without modifying verify.py, test.mjs, package.json, fixtures, or expected outputs. Correct value.txt to the verified value, robustly implement src/slugify.py while preserving its public API, and repair the counter transitions in src/counter.js. Run python verify.py and npm run test.";
    let planned = plan_typed(PlannerInput {
        objective: objective.to_owned(),
        attachment_count: 0,
        repository_paths: vec![
            "package.json".to_owned(),
            "src/counter.js".to_owned(),
            "src/slugify.py".to_owned(),
            "test.mjs".to_owned(),
            "value.txt".to_owned(),
            "verify.py".to_owned(),
        ],
    })
    .expect("live repair objective should plan");

    assert_eq!(planned.intent, PlanningIntent::MutationRequested);
    assert!(
        planned
            .scope
            .effective
            .iter()
            .any(|path| path == "src/counter.js"),
        "sentence punctuation must not remove the counter from mutation scope: {:?}",
        planned.scope
    );
    assert_eq!(planned.requested_outcomes, vec![objective.to_owned()]);
    planned
        .validate()
        .expect("path cleanup must preserve a valid planning fingerprint");
}

#[test]
fn common_terminal_prose_delimiters_resolve_existing_repository_paths() {
    for delimiter in [".", ",", ";", ":", ")", "]", "}", "!", "?"] {
        let objective = format!("Fix src/counter.js{delimiter}");
        let planned = plan_typed(PlannerInput {
            objective,
            attachment_count: 0,
            repository_paths: vec!["src/counter.js".to_owned()],
        })
        .expect("delimited repository path should plan");

        assert_eq!(planned.intent, PlanningIntent::MutationRequested);
        assert_eq!(planned.scope.effective, vec!["src/counter.js".to_owned()]);
    }
}

#[test]
fn exact_repository_path_with_terminal_period_wins_over_prose_fallback() {
    let planned = plan_typed(PlannerInput {
        objective: "Fix src/counter.js.".to_owned(),
        attachment_count: 0,
        repository_paths: vec!["src/counter.js".to_owned(), "src/counter.js.".to_owned()],
    })
    .expect("exact punctuated repository path should plan");

    assert_eq!(planned.scope.effective, vec!["src/counter.js.".to_owned()]);
}

#[test]
fn explanatory_how_to_language_remains_read_only() {
    let objective = "Explain how to fix src/lib.rs without changing files";
    let planned = plan_typed(PlannerInput {
        objective: objective.to_owned(),
        attachment_count: 0,
        repository_paths: vec!["src/lib.rs".to_owned()],
    })
    .expect("read-only objective should plan");

    assert_eq!(planned.intent, PlanningIntent::ReadOnly);
    assert_eq!(planned.strategy, ExecutionStrategy::CoordinatedReadOnly);
    assert!(planned.task(TaskKind::Implementation).is_none());
}

#[test]
fn inspection_without_modification_remains_read_only() {
    let planned = plan_typed(PlannerInput {
        objective: "Inspect src/lib.rs without modifying files".to_owned(),
        attachment_count: 0,
        repository_paths: vec!["src/lib.rs".to_owned()],
    })
    .expect("inspection objective should plan");

    assert_eq!(planned.intent, PlanningIntent::ReadOnly);
    assert_eq!(planned.strategy, ExecutionStrategy::CoordinatedReadOnly);
    assert!(planned.task(TaskKind::Implementation).is_none());
}

#[test]
fn direct_fix_can_protect_other_files_without_losing_write_authority() {
    let planned = plan_typed(PlannerInput {
        objective: "Fix src/lib.rs without modifying tests/regression.rs".to_owned(),
        attachment_count: 0,
        repository_paths: vec!["src/lib.rs".to_owned(), "tests/regression.rs".to_owned()],
    })
    .expect("scoped protection objective should plan");

    assert_eq!(planned.intent, PlanningIntent::MutationRequested);
    assert_eq!(planned.strategy, ExecutionStrategy::CoordinatedMutation);
    assert!(planned.task(TaskKind::Implementation).is_some());
}
