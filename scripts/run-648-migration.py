from __future__ import annotations

import subprocess
import sys
from pathlib import Path


def replace_once(text: str, before: str, after: str, label: str) -> str:
    if text.count(before) != 1:
        raise RuntimeError(f"{label} did not match exactly once")
    return text.replace(before, after, 1)


# Repair the original one-shot generator so it applies every cancellation checkpoint
# and appends scheduler tests to the actual test module rather than the first brace.
generator = Path("scripts/apply-648-scheduler.py")
text = generator.read_text(encoding="utf-8")
text = replace_once(
    text,
    '''def replace_once(path: Path, before: str, after: str) -> None:
    text = path.read_text(encoding="utf-8")
    if text.count(before) != 1:
        raise RuntimeError(f"{path}: expected exactly one match for replacement")
    path.write_text(text.replace(before, after, 1), encoding="utf-8")
''',
    '''def replace_once(path: Path, before: str, after: str) -> None:
    text = path.read_text(encoding="utf-8")
    count = text.count(before)
    all_cancellation_checkpoints = "cancel_requested(cancel, submission)" in before
    if count == 0 or (count != 1 and not all_cancellation_checkpoints):
        raise RuntimeError(
            f"{path}: expected one match, or all cancellation checkpoints; found {count}"
        )
    path.write_text(text.replace(before, after), encoding="utf-8")
''',
    "generator replacement helper",
)
text = replace_once(
    text,
    'replace_once(scheduler, "\\n}\\n", scheduler_tests + "\\n}\\n")',
    '''text = scheduler.read_text(encoding="utf-8")
position = text.rfind("\\n}\\n")
if position < 0:
    raise RuntimeError("scheduler test module closing brace was not found")
scheduler.write_text(text[:position] + scheduler_tests + text[position:], encoding="utf-8")''',
    "scheduler test insertion",
)
generator.write_text(text, encoding="utf-8")
subprocess.run([sys.executable, str(generator)], check=True)

# Complete the generated scheduler contract with deterministic projections and
# the explicit mutation vocabulary exercised by the acceptance benchmarks.
cargo = Path("crates/medusa-multi-agent-scheduler/Cargo.toml")
text = cargo.read_text(encoding="utf-8")
text = replace_once(
    text,
    "\n[lints]\nworkspace = true\n",
    "\n[dev-dependencies]\ntempfile.workspace = true\n\n[lints]\nworkspace = true\n",
    "scheduler tempfile dev-dependency",
)
cargo.write_text(text, encoding="utf-8")

scheduler = Path("crates/medusa-multi-agent-scheduler/src/lib.rs")
text = scheduler.read_text(encoding="utf-8")
start_marker = "    let mutation_requested = !explicitly_read_only\n"
end_marker = "        });\n    let repository_relevant ="
start = text.find(start_marker)
end = text.find(end_marker, start + len(start_marker))
if start < 0 or end < 0:
    raise RuntimeError("mutation intent expression boundaries were not found")
text = (
    text[:start]
    + "    let mutation_requested = !explicitly_read_only && contains_mutation_verb(&words);\n"
    + "    let repository_relevant ="
    + text[end + len(end_marker) :]
)
text = replace_once(
    text,
    "fn contains_phrase(value: &str, phrases: &[&str]) -> bool {\n",
    '''fn contains_mutation_verb(words: &BTreeSet<String>) -> bool {
    const VERBS: &[&str] = &[
        "add", "build", "change", "correct", "create", "delete", "edit", "fix",
        "implement", "make", "migrate", "modify", "patch", "refactor", "remove",
        "rename", "repair", "replace", "rewrite", "update", "upgrade", "write",
    ];
    VERBS.iter().any(|verb| words.contains(*verb))
}

fn contains_phrase(value: &str, phrases: &[&str]) -> bool {
''',
    "mutation vocabulary",
)
text = replace_once(
    text,
    "    #[test]\n    fn typed_planner_fails_closed_when_mutation_scope_is_unknown() {\n",
    '''    #[test]
    fn mutation_vocabulary_recognizes_fix() {
        let words = lexical_words("fix the failing tests");
        assert!(contains_mutation_verb(&words));
    }

    #[test]
    fn typed_planner_fails_closed_when_mutation_scope_is_unknown() {
''',
    "mutation vocabulary regression test",
)
for before, after, label in (
    (
        "struct LedgerTaskDefinition {\n    id: String,",
        "struct LedgerTaskDefinition {\n    order: u32,\n    id: String,",
        "ledger order field",
    ),
    (
        ".tasks\n            .iter()\n            .map(|planned| {",
        ".tasks\n            .iter()\n            .enumerate()\n            .map(|(order, planned)| {",
        "ledger ordered iterator",
    ),
    (
        "LedgerTaskDefinition {\n                        id: planned.task.id.clone(),",
        "LedgerTaskDefinition {\n                        order: u32::try_from(order).unwrap_or(u32::MAX),\n                        id: planned.task.id.clone(),",
        "ledger order value",
    ),
):
    text = replace_once(text, before, after, label)

views_start_marker = "    #[must_use]\n    pub fn views(&self) -> Vec<LedgerTaskView> {\n"
views_end_marker = "\n    #[must_use]\n    pub fn path(&self) -> &Path {"
views_start = text.find(views_start_marker)
views_end = text.find(views_end_marker, views_start + len(views_start_marker))
if views_start < 0 or views_end < 0:
    raise RuntimeError("ledger projection function boundaries were not found")
views = '''    #[must_use]
    pub fn views(&self) -> Vec<LedgerTaskView> {
        let mut definitions = self.state.tasks.values().collect::<Vec<_>>();
        definitions.sort_by_key(|definition| definition.order);
        definitions
            .into_iter()
            .filter_map(|definition| {
                self.state.states.get(&definition.id).cloned().map(|state| {
                    LedgerTaskView {
                        id: definition.id.clone(),
                        title: definition.title.clone(),
                        kind: definition.kind,
                        state,
                    }
                })
            })
            .collect()
    }
'''
text = text[:views_start] + views + text[views_end:]
scheduler.write_text(text, encoding="utf-8")

subprocess.run([sys.executable, "scripts/fix-648-ledger-scope.py"], check=True)
