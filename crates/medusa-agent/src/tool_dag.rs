use std::collections::{BTreeSet, VecDeque};

use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResourceAccess {
    Read,
    Write,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ToolResource {
    pub key: String,
    pub access: ResourceAccess,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SideEffectClass {
    None,
    Evidence,
    RepositoryMutation,
    RuntimeMutation,
    External,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ToolExecutionProfile {
    pub resources: Vec<ToolResource>,
    pub side_effect: SideEffectClass,
    pub idempotent: bool,
    pub cancellation_supported: bool,
    pub expected_duration_ms: u64,
    pub concurrency_cost: u16,
    pub priority: u8,
    pub parallel_safe: bool,
    pub dedup_key: Option<String>,
}

pub(crate) fn profile(name: &str, input: &Value) -> ToolExecutionProfile {
    let read = |key: &str| ToolResource {
        key: key.to_owned(),
        access: ResourceAccess::Read,
    };
    let write = |key: &str| ToolResource {
        key: key.to_owned(),
        access: ResourceAccess::Write,
    };
    let path = input.get("path").and_then(Value::as_str).unwrap_or("*");
    let (
        resources,
        side_effect,
        idempotent,
        cancellation_supported,
        expected_duration_ms,
        concurrency_cost,
        parallel_safe,
    ) = match name {
        "fs_read" => (
            vec![read("repository"), read(&format!("path:{path}"))],
            SideEffectClass::None,
            true,
            true,
            10,
            1,
            true,
        ),
        "search_text" => (
            vec![read("repository")],
            SideEffectClass::None,
            true,
            true,
            25,
            1,
            true,
        ),
        "skill_read" => (
            vec![read("repository")],
            SideEffectClass::None,
            true,
            true,
            10,
            1,
            true,
        ),
        "skill_execute" => (
            vec![read("repository"), write("process_pool")],
            SideEffectClass::External,
            false,
            true,
            5_000,
            4,
            false,
        ),
        "semantic_capabilities" => (
            vec![read("capabilities")],
            SideEffectClass::None,
            true,
            true,
            5,
            1,
            true,
        ),
        "code_index" | "inspect_target" => (
            vec![read("repository"), read("repository_graph")],
            SideEffectClass::None,
            true,
            true,
            30,
            1,
            true,
        ),
        "web_search" | "web_fetch" => (
            vec![read("network")],
            SideEffectClass::None,
            true,
            true,
            500,
            2,
            true,
        ),
        "verify_impacted" => (
            vec![
                read("repository"),
                write("verification_evidence"),
                write("process_pool"),
            ],
            SideEffectClass::Evidence,
            false,
            true,
            5_000,
            4,
            false,
        ),
        "fs_write"
        | "fs_create_dir"
        | "patch_apply"
        | "symbol_rename"
        | "apply_structured_patch"
        | "git_checkpoint" => (
            vec![write("repository")],
            SideEffectClass::RepositoryMutation,
            false,
            false,
            100,
            2,
            false,
        ),
        "shell_run" => (
            vec![write("repository"), write("process_pool")],
            SideEffectClass::External,
            false,
            true,
            5_000,
            4,
            false,
        ),
        "typescript_semantic" => (
            vec![read("repository"), write("language_server")],
            SideEffectClass::Evidence,
            false,
            true,
            250,
            2,
            false,
        ),
        "update_plan" | "ask_user_question" | "desktop_commander" => (
            vec![write("runtime")],
            SideEffectClass::RuntimeMutation,
            false,
            false,
            10,
            1,
            false,
        ),
        _ => (
            vec![write("runtime")],
            SideEffectClass::RuntimeMutation,
            false,
            false,
            100,
            1,
            false,
        ),
    };
    let priority = match name {
        "fs_read" | "search_text" | "skill_read" | "semantic_capabilities" => 3,
        "code_index" | "inspect_target" | "typescript_semantic" => 2,
        "web_search" | "web_fetch" => 1,
        _ => 2,
    };
    let dedup_key = (idempotent && side_effect == SideEffectClass::None).then(|| {
        format!(
            "{name}:{}",
            serde_json::to_string(input).unwrap_or_default()
        )
    });
    ToolExecutionProfile {
        resources,
        side_effect,
        idempotent,
        cancellation_supported,
        expected_duration_ms,
        concurrency_cost,
        priority,
        parallel_safe,
        dedup_key,
    }
}

pub(crate) fn dedup_key(name: &str, input: &Value) -> Option<String> {
    profile(name, input).dedup_key
}

pub(crate) fn invalidates_repository_revision(name: &str, input: &Value) -> bool {
    profile(name, input)
        .resources
        .iter()
        .any(|resource| resource.key == "repository" && resource.access == ResourceAccess::Write)
}

pub(crate) fn drain_stale_revision_calls<I>(
    queue: &mut VecDeque<(I, String, Value)>,
    current_revision: &str,
) -> Vec<(I, String, Value)> {
    let mut stale = Vec::new();
    let mut retained = VecDeque::new();
    for (id, name, input) in std::mem::take(queue) {
        let expected = input
            .get("repository_revision")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if expected.is_some_and(|expected| expected != current_revision) {
            stale.push((id, name, input));
        } else {
            retained.push_back((id, name, input));
        }
    }
    *queue = retained;
    stale
}

fn conflicts(left: &ToolExecutionProfile, right: &ToolExecutionProfile) -> bool {
    left.resources.iter().any(|left_resource| {
        right.resources.iter().any(|right_resource| {
            left_resource.key == right_resource.key
                && (left_resource.access == ResourceAccess::Write
                    || right_resource.access == ResourceAccess::Write)
        })
    })
}

fn depends_on(prior: &ToolExecutionProfile, current: &ToolExecutionProfile) -> bool {
    if prior.dedup_key.is_some() && prior.dedup_key == current.dedup_key {
        return true;
    }
    conflicts(prior, current)
}

/// Returns the next deterministic ready wave from the resource dependency DAG.
/// Independent safe reads can leap over a blocked mutation, while conflicts and
/// duplicate safe reads retain their original dependency order. `concurrency_budget`
/// is a weighted resource budget, not a call count: each selected profile consumes
/// its declared `concurrency_cost`, preventing expensive tools from oversubscribing
/// the executor. The first ready call is always admitted so progress remains possible
/// even when one tool's declared cost exceeds the configured budget.
pub(crate) fn select_ready_positions(
    calls: &[(String, Value)],
    concurrency_budget: usize,
) -> Vec<usize> {
    if calls.is_empty() {
        return Vec::new();
    }
    let profiles = calls
        .iter()
        .map(|(name, input)| profile(name, input))
        .collect::<Vec<_>>();
    let ready = (0..profiles.len())
        .filter(|&current| {
            !(0..current).any(|prior| depends_on(&profiles[prior], &profiles[current]))
        })
        .collect::<Vec<_>>();
    let Some(&first) = ready.first() else {
        return vec![0];
    };
    if !profiles[first].parallel_safe {
        return vec![first];
    }
    let barrier = ready
        .iter()
        .copied()
        .find(|&position| !profiles[position].parallel_safe)
        .unwrap_or(usize::MAX);
    let budget = concurrency_budget.max(1);
    let first_cost = usize::from(profiles[first].concurrency_cost.max(1));
    let mut consumed = first_cost;
    let mut selected = vec![first];
    if first_cost >= budget {
        return selected;
    }
    let mut candidates = ready
        .into_iter()
        .filter(|&position| {
            position != first && position < barrier && profiles[position].parallel_safe
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        profiles[*right]
            .priority
            .cmp(&profiles[*left].priority)
            .then(left.cmp(right))
    });
    for position in candidates {
        let cost = usize::from(profiles[position].concurrency_cost.max(1));
        if consumed.saturating_add(cost) <= budget {
            consumed = consumed.saturating_add(cost);
            selected.push(position);
        }
    }
    selected.sort_unstable();
    selected
}

pub(crate) fn drain_failed_dependents<I>(
    queue: &mut VecDeque<(I, String, Value)>,
    failed: &[(String, Value)],
) -> Vec<(I, String, Value)> {
    if queue.is_empty() || failed.is_empty() {
        return Vec::new();
    }
    let mut blockers = failed
        .iter()
        .map(|(name, input)| profile(name, input))
        .collect::<Vec<_>>();
    let mut blocked = Vec::new();
    let mut retained = VecDeque::new();
    for (id, name, input) in std::mem::take(queue) {
        let current = profile(&name, &input);
        if blockers.iter().any(|prior| depends_on(prior, &current)) {
            blockers.push(current);
            blocked.push((id, name, input));
        } else {
            retained.push_back((id, name, input));
        }
    }
    *queue = retained;
    blocked
}

pub(crate) fn drain_positions<T>(queue: &mut VecDeque<T>, positions: &[usize]) -> Vec<T> {
    let selected = positions.iter().copied().collect::<BTreeSet<_>>();
    let mut picked = Vec::with_capacity(selected.len());
    let mut retained = VecDeque::new();
    for (index, item) in std::mem::take(queue).into_iter().enumerate() {
        if selected.contains(&index) {
            picked.push((index, item));
        } else {
            retained.push_back(item);
        }
    }
    *queue = retained;
    picked.sort_by_key(|(index, _)| *index);
    picked.into_iter().map(|(_, item)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn calls(items: &[(&str, Value)]) -> Vec<(String, Value)> {
        items
            .iter()
            .map(|(name, input)| ((*name).to_owned(), input.clone()))
            .collect()
    }

    fn queued_calls(items: &[(&str, Value)]) -> VecDeque<(String, String, Value)> {
        calls(items)
            .into_iter()
            .enumerate()
            .map(|(index, (name, input))| (format!("call-{index}"), name, input))
            .collect()
    }

    #[test]
    fn independent_reads_form_one_ready_wave() {
        let calls = calls(&[
            ("fs_read", json!({"path":"a.rs"})),
            ("search_text", json!({"query":"needle"})),
            ("web_search", json!({"query":"docs"})),
        ]);
        assert_eq!(select_ready_positions(&calls, 8), vec![0, 1, 2]);
    }

    #[test]
    fn blocked_mutation_does_not_block_independent_network_read() {
        let calls = calls(&[
            ("fs_read", json!({"path":"a.rs"})),
            ("fs_write", json!({"path":"a.rs","content":"x"})),
            ("web_fetch", json!({"url":"https://example.com"})),
        ]);
        assert_eq!(select_ready_positions(&calls, 8), vec![0, 2]);
    }

    #[test]
    fn conflicting_mutations_serialize() {
        let calls = calls(&[
            ("fs_write", json!({"path":"a.rs","content":"x"})),
            (
                "apply_structured_patch",
                json!({"repository_revision":"r","edits":[]}),
            ),
        ]);
        assert_eq!(select_ready_positions(&calls, 8), vec![0]);
    }

    #[test]
    fn duplicate_safe_read_waits_for_cacheable_predecessor() {
        let input = json!({"path":"a.rs"});
        let calls = calls(&[("fs_read", input.clone()), ("fs_read", input)]);
        assert_eq!(select_ready_positions(&calls, 8), vec![0]);
        assert!(dedup_key(&calls[0].0, &calls[0].1).is_some());
    }

    #[test]
    fn concurrency_cost_bounds_ready_wave() {
        let calls = calls(&[
            ("web_search", json!({"query":"one"})),
            ("web_fetch", json!({"url":"https://example.com/a"})),
            ("fs_read", json!({"path":"a.rs"})),
        ]);
        assert_eq!(select_ready_positions(&calls, 4), vec![0, 2]);
        assert_eq!(select_ready_positions(&calls, 5), vec![0, 1, 2]);
    }

    #[test]
    fn oversized_first_ready_call_is_admitted_for_progress() {
        let calls = calls(&[("web_search", json!({"query":"one"}))]);
        assert_eq!(select_ready_positions(&calls, 1), vec![0]);
    }

    #[test]
    fn skipped_expensive_read_does_not_block_later_affordable_read() {
        let calls = calls(&[
            ("fs_read", json!({"path":"a.rs"})),
            ("web_search", json!({"query":"one"})),
            ("search_text", json!({"query":"needle"})),
        ]);
        assert_eq!(select_ready_positions(&calls, 2), vec![0, 2]);
    }

    #[test]
    fn priority_favors_short_local_work_after_oldest_ready_call() {
        let calls = calls(&[
            ("web_search", json!({"query":"one"})),
            ("web_fetch", json!({"url":"https://example.com/a"})),
            ("fs_read", json!({"path":"a.rs"})),
        ]);
        assert_eq!(select_ready_positions(&calls, 4), vec![0, 2]);
    }

    #[test]
    fn oldest_ready_call_is_never_starved_by_later_priority_work() {
        let calls = calls(&[
            ("web_search", json!({"query":"oldest"})),
            ("fs_read", json!({"path":"a.rs"})),
            ("search_text", json!({"query":"needle"})),
        ]);
        assert_eq!(select_ready_positions(&calls, 2), vec![0]);
    }

    #[test]
    fn failed_dependency_blocks_conflicting_downstream_calls_transitively() {
        let mut queue = queued_calls(&[
            ("fs_write", json!({"path":"a.rs","content":"x"})),
            ("fs_read", json!({"path":"b.rs"})),
            (
                "apply_structured_patch",
                json!({"repository_revision":"r","edits":[]}),
            ),
            ("web_fetch", json!({"url":"https://example.com"})),
        ]);
        let failed = calls(&[("fs_read", json!({"path":"a.rs"}))]);
        let blocked = drain_failed_dependents(&mut queue, &failed);
        assert_eq!(
            blocked
                .iter()
                .map(|(_, name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["fs_write", "fs_read", "apply_structured_patch"]
        );
        assert_eq!(
            queue
                .iter()
                .map(|(_, name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["web_fetch"]
        );
    }

    #[test]
    fn failed_network_dependency_does_not_cancel_repository_work() {
        let mut queue = queued_calls(&[
            ("fs_read", json!({"path":"a.rs"})),
            ("web_fetch", json!({"url":"https://example.com/next"})),
        ]);
        let failed = calls(&[("web_search", json!({"query":"docs"}))]);
        let blocked = drain_failed_dependents(&mut queue, &failed);
        assert!(blocked.is_empty());
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn stale_revision_calls_are_pruned_without_touching_matching_or_unbound_work() {
        let mut queue = queued_calls(&[
            (
                "verify_impacted",
                json!({"repository_revision":"old","paths":["a.rs"]}),
            ),
            (
                "verify_impacted",
                json!({"repository_revision":"current","paths":["b.rs"]}),
            ),
            ("web_fetch", json!({"url":"https://example.com"})),
            (
                "apply_structured_patch",
                json!({"repository_revision":"older","plan_id":"p","edits":[]}),
            ),
        ]);
        let stale = drain_stale_revision_calls(&mut queue, "current");
        assert_eq!(
            stale
                .iter()
                .map(|(_, name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["verify_impacted", "apply_structured_patch"]
        );
        assert_eq!(
            queue
                .iter()
                .map(|(_, name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["verify_impacted", "web_fetch"]
        );
    }

    #[test]
    fn scheduler_profiles_identify_repository_revision_invalidators() {
        assert!(invalidates_repository_revision(
            "apply_structured_patch",
            &json!({"repository_revision":"r"})
        ));
        assert!(invalidates_repository_revision(
            "shell_run",
            &json!({"program":"cargo","args":["fmt"]})
        ));
        assert!(!invalidates_repository_revision(
            "inspect_target",
            &json!({"path":"src/lib.rs"})
        ));
        assert!(!invalidates_repository_revision(
            "verify_impacted",
            &json!({"repository_revision":"r","paths":["src/lib.rs"]})
        ));
    }

    #[test]
    fn non_idempotent_calls_never_have_dedup_keys() {
        assert!(dedup_key("fs_write", &json!({"path":"a","content":"x"})).is_none());
        assert!(dedup_key("shell_run", &json!({"program":"cargo","args":["test"]})).is_none());
    }

    #[test]
    fn drain_preserves_deterministic_original_order() {
        let mut queue = VecDeque::from(vec!["a", "b", "c", "d"]);
        assert_eq!(drain_positions(&mut queue, &[2, 0]), vec!["a", "c"]);
        assert_eq!(queue, VecDeque::from(vec!["b", "d"]));
    }

    #[test]
    fn profiles_expose_scheduler_contract() {
        let read = profile("inspect_target", &json!({"path":"src/lib.rs"}));
        assert_eq!(read.side_effect, SideEffectClass::None);
        assert!(read.idempotent && read.parallel_safe && read.cancellation_supported);
        assert_eq!(read.priority, 2);
        let mutation = profile("apply_structured_patch", &json!({}));
        assert_eq!(mutation.side_effect, SideEffectClass::RepositoryMutation);
        assert!(!mutation.idempotent && !mutation.parallel_safe);
    }
}
