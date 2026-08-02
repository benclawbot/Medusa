#!/usr/bin/env python3
from pathlib import Path

coordinator = Path('crates/medusa-runtime/src/mutating_worker_coordinator.rs')
text = coordinator.read_text(encoding='utf-8')

helper_anchor = '''fn component_paths(components: &[ChangedComponent]) -> Vec<String> {
    let mut paths = components
        .iter()
        .flat_map(ChangedComponent::all_paths)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}
'''
helper = helper_anchor + '''
fn has_in_scope_repository_changes(
    worker: &Worker,
    allowed_write_paths: &[String],
) -> Result<bool, String> {
    if allowed_write_paths.is_empty() {
        return Ok(false);
    }
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all", "--"])
        .args(allowed_write_paths)
        .current_dir(&worker.worktree)
        .output()
        .map_err(|error| {
            format!(
                "failed to inspect bounded implementer worktree {}: {error}",
                worker.worktree.display()
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "git status failed while inspecting bounded implementer worktree: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(!output.stdout.is_empty())
}
'''
if text.count(helper_anchor) != 1:
    raise SystemExit(f'component helper anchor count={text.count(helper_anchor)}')
text = text.replace(helper_anchor, helper, 1)

function_start = text.index('fn execute_production_implementer(')
block_start = text.index('    if !completed {', function_start)
block_end_marker = '''    Ok(WorkerRun {
        session_id: session.id.to_string(),
        turns: session.turn,
        summary,
    })
'''
block_end = text.index(block_end_marker, block_start) + len(block_end_marker)
replacement = '''    let mut summary = summaries.join("\\n").trim().to_owned();
    if !completed {
        if !has_in_scope_repository_changes(
            &request.worker,
            &request.contract.allowed_write_paths,
        )? {
            return Err(format!(
                "worker {} exceeded its bounded turn budget without producing an in-scope repository change",
                request.worker.id
            ));
        }
        let fallback = "The model exhausted its bounded turn budget after producing an in-scope repository change. Runtime exact-scope validation and independent verification are authoritative; this narrative is advisory.";
        if summary.is_empty() {
            summary = fallback.to_owned();
        } else {
            summary.push_str("\\n\\n");
            summary.push_str(fallback);
        }
    }
    if summary.is_empty() {
        return Err(format!(
            "worker {} returned no implementation evidence",
            request.worker.id
        ));
    }
    Ok(WorkerRun {
        session_id: session.id.to_string(),
        turns: session.turn,
        summary,
    })
'''
text = text[:block_start] + replacement + text[block_end:]
coordinator.write_text(text, encoding='utf-8')

tests = Path('crates/medusa-runtime/src/mutating_worker_coordinator_tests.rs')
text = tests.read_text(encoding='utf-8')
test_anchor = '''#[test]
fn implementer_turn_budget_is_bounded_without_truncating_smaller_limits() {
'''
new_test = '''#[test]
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
        "pub fn value() -> u32 { 2 }\\n",
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

''' + test_anchor
if text.count(test_anchor) != 1:
    raise SystemExit(f'test anchor count={text.count(test_anchor)}')
tests.write_text(text.replace(test_anchor, new_test, 1), encoding='utf-8')
