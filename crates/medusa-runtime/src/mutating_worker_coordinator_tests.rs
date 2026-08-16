use std::sync::mpsc;

use crate::{
    production_orchestrator::{self, AgentRole},
    prompt::PromptDraft,
};

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
            crate::multi_agent_coordinator::WorkerEvidence {
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
            crate::multi_agent_coordinator::WorkerEvidence {
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
    let manager = WorkerManager::new(
        &repo,
        repo.join(".medusa/executions/turn-budget/worktrees"),
    )
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
