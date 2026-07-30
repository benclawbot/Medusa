from pathlib import Path

coordinator = Path("crates/medusa-runtime/src/mutating_worker_coordinator.rs")
source = coordinator.read_text()
old_constants = '''const LEASE_TIMEOUT_MS: u64 = 10 * 60 * 1_000;
const IMPLEMENTER_TURN_LIMIT: u32 = 12;
const MAX_ATTEMPTS: u32 = 2;
const IMPLEMENTER_ID: &str = "worker-implement";
'''
new_constants = '''const LEASE_TIMEOUT_MS: u64 = 10 * 60 * 1_000;
// Keep implementation continuous and bounded. Restarting after a small ceiling discards the
// model's accumulated repository context and repeats the same work in a fresh worktree.
const IMPLEMENTER_TURN_LIMIT: u32 = 32;
const MAX_ATTEMPTS: u32 = 2;
const IMPLEMENTER_ID: &str = "worker-implement";
const TURN_BUDGET_EXHAUSTED: &str = "exceeded its bounded turn budget";
'''
assert source.count(old_constants) == 1
source = source.replace(old_constants, new_constants)
old_retry = '''                let cancelled = cancel.load(Ordering::SeqCst);
                let retryable = !cancelled && attempt < MAX_ATTEMPTS;
'''
new_retry = '''                let cancelled = cancel.load(Ordering::SeqCst);
                let retryable = executor_failure_retryable(&error, cancelled, attempt);
'''
assert source.count(old_retry) == 1
source = source.replace(old_retry, new_retry)
old_clamp = '''    worker_config.agent.max_turns = worker_config
        .agent
        .max_turns
        .clamp(1, IMPLEMENTER_TURN_LIMIT);
'''
new_clamp = '''    worker_config.agent.max_turns = bounded_implementer_turns(worker_config.agent.max_turns);
'''
assert source.count(old_clamp) == 1
source = source.replace(old_clamp, new_clamp)
old_error = '''        return Err(format!(
            "worker {} exceeded its bounded turn budget",
            request.worker.id
        ));
'''
new_error = '''        return Err(format!("worker {} {TURN_BUDGET_EXHAUSTED}", request.worker.id));
'''
assert source.count(old_error) == 1
source = source.replace(old_error, new_error)
anchor = '''fn execute_production_implementer(
'''
helpers = '''fn bounded_implementer_turns(configured: u32) -> u32 {
    configured.clamp(1, IMPLEMENTER_TURN_LIMIT)
}

fn executor_failure_retryable(error: &str, cancelled: bool, attempt: u32) -> bool {
    !cancelled && attempt < MAX_ATTEMPTS && !error.ends_with(TURN_BUDGET_EXHAUSTED)
}

'''
assert source.count(anchor) == 1
source = source.replace(anchor, helpers + anchor)
coordinator.write_text(source)

tests = Path("crates/medusa-runtime/src/mutating_worker_coordinator_tests.rs")
test_source = tests.read_text().rstrip()
assert test_source.endswith("}")
assert "turn_budget_exhaustion_is_not_retried" not in test_source
addition = '''

    #[test]
    fn implementer_turn_budget_is_continuous_and_bounded() {
        assert_eq!(bounded_implementer_turns(500), IMPLEMENTER_TURN_LIMIT);
        assert_eq!(bounded_implementer_turns(8), 8);
        assert_eq!(bounded_implementer_turns(0), 1);
    }

    #[test]
    fn turn_budget_exhaustion_is_not_retried() {
        let (_directory, repo, plan, preflight) = repository("src/");
        let cancel = Arc::new(AtomicBool::new(false));
        let (events, _) = mpsc::channel();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = std::sync::Arc::clone(&calls);
        let error = coordinate_with_executor(
            &repo,
            &plan,
            &preflight,
            &cancel,
            &events,
            move |_| {
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err(format!("worker {IMPLEMENTER_ID} {TURN_BUDGET_EXHAUSTED}"))
            },
        )
        .expect_err("turn ceiling must remain terminal");
        assert!(error.ends_with(TURN_BUDGET_EXHAUSTED));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
'''
test_source += addition + "\n"
tests.write_text(test_source)
