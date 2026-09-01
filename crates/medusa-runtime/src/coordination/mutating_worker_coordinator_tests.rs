use std::{cell::Cell, sync::mpsc};

use crate::coordination::production_orchestrator::{self, AgentRole};
use crate::prompt::PromptDraft;

use super::*;

fn git(repo: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository(
    scope: &str,
) -> (
    tempfile::TempDir,
    PathBuf,
    ProductionExecutionPlan,
    CoordinatorEvidence,
) {
    let directory = tempfile::tempdir().expect("tempdir");
    let repo = directory.path().join("repo");
    fs::create_dir(&repo).expect("repo");
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "core.autocrlf", "false"]);
    git(&repo, &["config", "user.name", "Medusa Test"]);
    git(&repo, &["config", "user.email", "medusa@example.invalid"]);
    fs::create_dir_all(repo.join("src")).expect("src");
    fs::write(repo.join("src/lib.rs"), "pub fn value() -> u32 { 1 }\n").expect("source");
    #[cfg(windows)]
    fs::write(
        repo.join("verify.ps1"),
        "$ErrorActionPreference='Stop'\nif (-not (Test-Path src/lib.rs)) { exit 1 }\nWrite-Output 'verification-ok'\n",
    )
    .expect("verify");
    #[cfg(not(windows))]
    fs::write(
        repo.join("verify.sh"),
        "#!/bin/sh\nset -eu\ntest -f src/lib.rs\necho verification-ok\n",
    )
    .expect("verify");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-m", "base"]);
    let plan = production_orchestrator::plan(&PromptDraft {
        text: format!("Implement the fix in {scope} with tests"),
        ..PromptDraft::default()
    })
    .expect("plan");
    let root = repo.join(".medusa/executions/test");
    fs::create_dir_all(&root).expect("root");
    let preflight = CoordinatorEvidence {
        plan_fingerprint: plan.fingerprint.clone(),
        repository_fingerprint: "repository-fingerprint".to_owned(),
        workers: vec![
            crate::coordination::multi_agent_coordinator::WorkerEvidence {
                task_id: "analyze".to_owned(),
                worker_id: "worker-analyze".to_owned(),
                role: AgentRole::Planner,
                context_fingerprint: "planner-context".to_owned(),
                lease_epoch: 1,
                delegation_contract_id: String::new(),
                delegation_contract_fingerprint: String::new(),
                delegation_attempt_fingerprint: String::new(),
                session_id: "planner-session".to_owned(),
                turns: 1,
                summary: "planner evidence".to_owned(),
            },
            crate::coordination::multi_agent_coordinator::WorkerEvidence {
                task_id: "risk-review".to_owned(),
                worker_id: "worker-risk-review".to_owned(),
                role: AgentRole::Researcher,
                context_fingerprint: "risk-context".to_owned(),
                lease_epoch: 1,
                delegation_contract_id: String::new(),
                delegation_contract_fingerprint: String::new(),
                delegation_attempt_fingerprint: String::new(),
                session_id: "risk-session".to_owned(),
                turns: 1,
                summary: "risk evidence".to_owned(),
            },
        ],
        state_path: root.join("preflight-evidence.json"),
    };
    (directory, repo, plan, preflight)
}

#[test]
fn turn_budget_fallback_requires_an_in_scope_repository_change() {
    let (_directory, repo, _plan, _preflight) = repository("src/");
    let manager = WorkerManager::new(&repo, repo.join(".medusa/executions/turn-budget/worktrees"))
        .expect("manager");
    let worker = manager
        .open_or_create_worker("turn-budget", "worker-turn-budget")
        .expect("worker");
    fs::write(
        worker.worktree.join("src/lib.rs"),
        "pub fn value() -> u32 { 2 }\n",
    )
    .expect("change");
    assert!(
        has_in_scope_repository_changes(&worker, &["src/".to_owned()])
            .expect("in-scope inspection")
    );
    assert!(
        !has_in_scope_repository_changes(&worker, &["docs/".to_owned()])
            .expect("out-of-scope inspection")
    );
    manager.cleanup(&[worker]).expect("cleanup");
}

#[test]
fn implementer_turn_budget_is_bounded_without_truncating_smaller_limits() {
    assert_eq!(bounded_implementer_turns(0), 1);
    assert_eq!(bounded_implementer_turns(8), 8);
    assert_eq!(
        bounded_implementer_turns(Config::default().agent.max_turns),
        IMPLEMENTER_TURN_LIMIT
    );
    assert_eq!(
        bounded_implementer_turns(IMPLEMENTER_TURN_LIMIT + 1),
        IMPLEMENTER_TURN_LIMIT
    );
}

#[test]
fn retry_budget_escalates_once_without_crossing_runtime_limits() {
    assert_eq!(adaptive_implementer_turns(2, 1, 24), 2);
    assert_eq!(adaptive_implementer_turns(2, 2, 24), 4);
    assert_eq!(adaptive_implementer_turns(2, 3, 24), 6);
    assert_eq!(adaptive_implementer_turns(20, 2, 24), 24);
    assert_eq!(adaptive_implementer_turns(2, 2, 3), 3);
    assert_eq!(adaptive_implementer_turns(0, 1, 24), 1);
}

#[test]
fn isolated_implementation_is_verified_prepared_and_preserved() {
    let (_directory, repo, plan, preflight) = repository("src/");
    let cancel = Arc::new(AtomicBool::new(false));
    let (events, _) = mpsc::channel();
    let evidence =
        coordinate_with_executor(&repo, &plan, &preflight, &cancel, &events, |request| {
            fs::write(
                request.worker.worktree.join("src/lib.rs"),
                "pub fn value() -> u32 { 2 }\n",
            )
            .map_err(|error| error.to_string())?;
            Ok(WorkerRun {
                session_id: "implementer-session".to_owned(),
                turns: 2,
                summary: "updated src/lib.rs and verified the change".to_owned(),
            })
        })
        .expect("implementation");
    assert_eq!(
        fs::read_to_string(repo.join("src/lib.rs")).unwrap(),
        "pub fn value() -> u32 { 1 }\n"
    );
    assert_eq!(evidence.changed_paths, vec!["src/lib.rs"]);
    assert!(!evidence.prepared_commit.is_empty());
    assert!(!evidence.prepared_tree.is_empty());
    assert!(evidence.transaction_path.is_file());
    assert!(repo.join(".medusa/executions/test/worktrees").exists());
}

#[test]
fn corrective_attempt_preserves_verified_partial_repairs() {
    let (_directory, repo, plan, preflight) = repository("src/");
    fs::write(repo.join("src/first.txt"), "one\n").expect("first fixture");
    fs::write(repo.join("src/value.txt"), "one\n").expect("value fixture");
    fs::write(repo.join("src/other.txt"), "one\n").expect("other fixture");
    #[cfg(windows)]
    fs::write(
        repo.join("verify.ps1"),
        "$ErrorActionPreference='Stop'\nif ((Get-Content src/first.txt -Raw).Trim() -ne 'two') { throw 'first assertion unresolved' }\nif ((Get-Content src/value.txt -Raw).Trim() -ne 'three') { throw 'value assertion unresolved' }\nif ((Get-Content src/other.txt -Raw).Trim() -ne 'four') { throw 'other assertion unresolved' }\nWrite-Output 'verification-ok'\n",
    )
    .expect("verify");
    #[cfg(not(windows))]
    fs::write(
        repo.join("verify.sh"),
        "#!/bin/sh\nset -eu\ngrep -Fxq 'two' src/first.txt\ngrep -Fxq 'three' src/value.txt\ngrep -Fxq 'four' src/other.txt\necho verification-ok\n",
    )
    .expect("verify");
    git(&repo, &["add", "-A"]);
    git(
        &repo,
        &["commit", "-m", "three independent failing assertions"],
    );

    let cancel = Arc::new(AtomicBool::new(false));
    let (events, _) = mpsc::channel();
    let attempts = Cell::new(0u32);
    let first_budget = Cell::new(0u32);
    let second_budget = Cell::new(0u32);
    let second_received_failure_feedback = Cell::new(false);
    let evidence =
        coordinate_with_executor(&repo, &plan, &preflight, &cancel, &events, |request| {
            let attempt = attempts.get() + 1;
            attempts.set(attempt);
            match attempt {
                1 => {
                    first_budget.set(request.max_model_turns);
                    fs::write(request.worker.worktree.join("src/first.txt"), "two\n")
                        .map_err(|error| error.to_string())?;
                }
                2 => {
                    second_budget.set(request.max_model_turns);
                    second_received_failure_feedback.set(request.prior_failure.is_some());
                    let retained =
                        fs::read_to_string(request.worker.worktree.join("src/first.txt"))
                            .map_err(|error| error.to_string())?;
                    if retained != "two\n" {
                        return Err(
                            "corrective attempt lost the verified partial repair".to_owned()
                        );
                    }
                    fs::write(request.worker.worktree.join("src/value.txt"), "three\n")
                        .map_err(|error| error.to_string())?;
                    fs::write(request.worker.worktree.join("src/other.txt"), "four\n")
                        .map_err(|error| error.to_string())?;
                }
                _ => return Err("unexpected extra implementation attempt".to_owned()),
            }
            Ok(WorkerRun {
                session_id: format!("implementer-session-{attempt}"),
                turns: 1,
                summary: format!("implementation attempt {attempt}"),
            })
        })
        .expect("corrective implementation");

    assert_eq!(attempts.get(), 2);
    assert!(second_budget.get() > first_budget.get());
    assert!(second_received_failure_feedback.get());
    assert_eq!(
        evidence.changed_paths,
        vec!["src/first.txt", "src/other.txt", "src/value.txt"]
    );
    assert!(evidence.verification_receipt.passed);
}

#[test]
fn interrupted_terminal_scheduler_is_reopened_for_bounded_recovery() {
    let (_directory, repo, plan, preflight) = repository("src/");
    let cancel = Arc::new(AtomicBool::new(false));
    let (events, _) = mpsc::channel();

    let first_error = coordinate_with_executor(&repo, &plan, &preflight, &cancel, &events, |_| {
        Err("simulated provider interruption".to_owned())
    })
    .expect_err("first attempt should fail");
    assert!(first_error.contains("simulated provider interruption"));

    let state_path = preflight
        .state_path
        .parent()
        .unwrap()
        .join("implementation-state.json");
    let mut state = load_state(&state_path).expect("durable failure state");
    assert_eq!(state.status, ImplementationStatus::Failed);
    // Model a process interruption between durable child failure and the
    // parent state transition. The next invocation must recover this boundary
    // instead of dispatching a terminal child task and returning zero work.
    state.status = ImplementationStatus::Running;
    write_atomic(&state_path, &state).expect("simulate interrupted state");

    let evidence =
        coordinate_with_executor(&repo, &plan, &preflight, &cancel, &events, |request| {
            fs::write(
                request.worker.worktree.join("src/lib.rs"),
                "pub fn value() -> u32 { 2 }\n",
            )
            .map_err(|error| error.to_string())?;
            Ok(WorkerRun {
                session_id: "recovery-session".to_owned(),
                turns: 1,
                summary: "recovered after reopening the bounded scheduler".to_owned(),
            })
        })
        .expect("terminal scheduler should be reopened");

    assert_eq!(evidence.changed_paths, vec!["src/lib.rs"]);
}

#[test]
fn out_of_scope_change_is_rejected_without_touching_primary_repo() {
    let (_directory, repo, plan, preflight) = repository("src/");
    let cancel = Arc::new(AtomicBool::new(false));
    let (events, _) = mpsc::channel();
    let error = coordinate_with_executor(&repo, &plan, &preflight, &cancel, &events, |request| {
        fs::write(request.worker.worktree.join("outside.txt"), "blocked\n")
            .map_err(|error| error.to_string())?;
        Ok(WorkerRun {
            session_id: "implementer-session".to_owned(),
            turns: 1,
            summary: "attempted change".to_owned(),
        })
    })
    .expect_err("scope must fail");
    assert!(error.contains("out-of-scope"));
    assert!(!repo.join("outside.txt").exists());
}

#[test]
fn cancellation_prevents_implementation_dispatch() {
    let (_directory, repo, plan, preflight) = repository("src/");
    let cancel = Arc::new(AtomicBool::new(true));
    let (events, _) = mpsc::channel();
    let error = coordinate_with_executor(&repo, &plan, &preflight, &cancel, &events, |_| {
        Err("executor must not run".to_owned())
    })
    .expect_err("cancelled");
    assert!(error.contains("cancelled"));
    assert!(!repo.join(".medusa/executions/test/worktrees").exists());
}

#[test]
fn promotion_conflict_is_fail_closed_on_explicit_risk_escalation() {
    let (_directory, _repo, _plan, mut preflight) = repository("src/");
    assert!(preflight_promotion_conflict(&preflight).is_none());
    preflight
        .workers
        .iter_mut()
        .find(|worker| worker.task_id == "risk-review")
        .expect("risk worker")
        .summary = "Scope expansion required before implementation can be accepted.".to_owned();
    let conflict = preflight_promotion_conflict(&preflight).expect("promotion conflict");
    assert!(conflict.contains("scope expansion required"));
}
