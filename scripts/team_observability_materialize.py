#!/usr/bin/env python3
"""One-shot source materializer for the validated team observability slice."""

from __future__ import annotations

import subprocess
import urllib.request
from pathlib import Path

BRANCH = "agent/team-observability-steering"
RAW = f"https://raw.githubusercontent.com/benclawbot/Medusa/{BRANCH}"
CHECKER = Path("scripts/check-product-architecture.py")
HOOK_START = "# BEGIN TEAM OBSERVABILITY MATERIALIZER\n"
HOOK_END = "# END TEAM OBSERVABILITY MATERIALIZER\n"


def download(path: str) -> str:
    with urllib.request.urlopen(f"{RAW}/{path}", timeout=30) as response:
        return response.read().decode("utf-8")


def edit_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    source = file.read_text()
    count = source.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one anchor, found {count}: {old[:100]!r}")
    file.write_text(source.replace(old, new, 1))


def remove_exact(source: str, snippet: str, expected: int) -> str:
    count = source.count(snippet)
    if count != expected:
        raise SystemExit(
            f"embedded materializer: expected {expected} occurrences, found {count}: {snippet[:100]!r}"
        )
    return source.replace(snippet, "")


Path("crates/medusa-runtime/src/team_control.rs").write_text(
    download("crates/medusa-runtime/src/team_control.rs")
)
Path("docs/TEAM-OBSERVABILITY-STEERING.md").write_text(
    download("docs/TEAM-OBSERVABILITY-STEERING.md")
)

workflow = download(".github/workflows/apply-team-observability.yml").splitlines()
start = next(index for index, line in enumerate(workflow) if "python3 - <<'PY'" in line) + 1
end = next(index for index in range(start, len(workflow)) if workflow[index].strip() == "PY")
source = "\n".join(
    line[10:] if line.startswith("          ") else line for line in workflow[start:end]
) + "\n"

build_block_start = source.index("edit('crates/medusa-runtime/build_main.rs', [")
build_block_end = source.index(
    "\n\nedit('crates/medusa-runtime/src/multi_agent_coordinator.rs',",
    build_block_start,
)
source = source[:build_block_start] + source[build_block_end + 2 :]
source = remove_exact(
    source,
    "    ('fn coordinate_with_executor<F>(\\n', 'fn coordinate_with_control<F>(\\n'),\n",
    1,
)
source = remove_exact(
    source,
    "    ('    cancel: &Arc<AtomicBool>,\\n    events: &Sender<RuntimeEvent>,\\n    executor: F,\\n', '    cancel: &Arc<AtomicBool>,\\n    control: &TeamControlPlane,\\n    events: &Sender<RuntimeEvent>,\\n    executor: F,\\n'),\n",
    2,
)
source = remove_exact(
    source,
    "    ('        if cancel.load(Ordering::SeqCst) {', '        if cancel.load(Ordering::SeqCst) || control.is_cancelled(IMPLEMENTER_ID) {'),\n",
    1,
)
exec(compile(source, "team-observability-materializer.py", "exec"), {})

edit_once(
    "crates/medusa-runtime/src/multi_agent_coordinator.rs",
    """fn coordinate_with_executor<F>(
    repo: &Path,
    plan: &ProductionExecutionPlan,
    cancel: &Arc<AtomicBool>,
    events: &Sender<RuntimeEvent>,
    executor: F,
) -> Result<CoordinatorEvidence, String>
where
    F: Fn(WorkerRequest) -> Result<WorkerEvidence, String> + Sync,
{
    let repository_fingerprint = repository_fingerprint(repo)?;
""",
    """fn coordinate_with_control<F>(
    repo: &Path,
    plan: &ProductionExecutionPlan,
    cancel: &Arc<AtomicBool>,
    control: &TeamControlPlane,
    events: &Sender<RuntimeEvent>,
    executor: F,
) -> Result<CoordinatorEvidence, String>
where
    F: Fn(WorkerRequest) -> Result<WorkerEvidence, String> + Sync,
{
    let repository_fingerprint = repository_fingerprint(repo)?;
""",
)
edit_once(
    "crates/medusa-runtime/src/mutating_worker_coordinator.rs",
    """fn coordinate_with_control<F>(
    repo: &Path,
    plan: &ProductionExecutionPlan,
    preflight: &CoordinatorEvidence,
    cancel: &Arc<AtomicBool>,
    events: &Sender<RuntimeEvent>,
    executor: F,
) -> Result<ImplementationEvidence, String>
where
    F: Fn(ImplementationRequest) -> Result<WorkerRun, String>,
{
    validate_preflight(plan, preflight)?;
""",
    """fn coordinate_with_control<F>(
    repo: &Path,
    plan: &ProductionExecutionPlan,
    preflight: &CoordinatorEvidence,
    cancel: &Arc<AtomicBool>,
    control: &TeamControlPlane,
    events: &Sender<RuntimeEvent>,
    executor: F,
) -> Result<ImplementationEvidence, String>
where
    F: Fn(ImplementationRequest) -> Result<WorkerRun, String>,
{
    validate_preflight(plan, preflight)?;
""",
)
edit_once(
    "crates/medusa-runtime/src/mutating_worker_coordinator.rs",
    """    for attempt in 1..=MAX_ATTEMPTS {
        if cancel.load(Ordering::SeqCst) {
""",
    """    for attempt in 1..=MAX_ATTEMPTS {
        if cancel.load(Ordering::SeqCst) || control.is_cancelled(IMPLEMENTER_ID) {
""",
)

edit_once(
    "crates/medusa-runtime/build_main.rs",
    '    println!("cargo:rerun-if-changed=src/multi_agent_coordinator.rs");\n',
    '    println!("cargo:rerun-if-changed=src/multi_agent_coordinator.rs");\n'
    '    println!("cargo:rerun-if-changed=src/team_control.rs");\n',
)
edit_once(
    "crates/medusa-runtime/build_main.rs",
    '        ("mod multi_agent_coordinator;", "multi_agent_coordinator.rs"),\n',
    '        ("mod multi_agent_coordinator;", "multi_agent_coordinator.rs"),\n'
    '        ("mod team_control;", "team_control.rs"),\n',
)
late_wiring = r'''    replace_once(
        &mut source,
        "    let execution_plan = crate::production_orchestrator::plan(&draft).map_err(RuntimeError::agent)?;\n    for event",
        "    let execution_plan = crate::production_orchestrator::plan(&draft).map_err(RuntimeError::agent)?;\n    if execution_plan.mode == crate::production_orchestrator::ExecutionMode::Direct { let _ = events.send(RuntimeEvent::Team(state.team_control.clear())); }\n    for event",
    )?;
    replace_once(
        &mut source,
        "                cancel,\n                events,\n            )\n            .map_err(RuntimeError::agent)?,\n        )\n    } else {\n        None\n    };\n    let coordinated",
        "                cancel,\n                &state.team_control,\n                events,\n            )\n            .map_err(RuntimeError::agent)?,\n        )\n    } else {\n        None\n    };\n    let coordinated",
    )?;
    replace_once(
        &mut source,
        "                preflight,\n                cancel,\n                events,\n            )",
        "                preflight,\n                cancel,\n                &state.team_control,\n                events,\n            )",
    )?;
    replace_once(
        &mut source,
        "    state.session = Some(session);\n    result\n}\n\nfn append_followups",
        "    if coordinated { let _ = events.send(RuntimeEvent::Team(state.team_control.finish())); }\n    state.session = Some(session);\n    result\n}\n\nfn append_followups",
    )?;

'''
edit_once(
    "crates/medusa-runtime/build_main.rs",
    '    source = source.replace("cancel: &AtomicBool", "cancel: &Arc<AtomicBool>");\n',
    late_wiring
    + '    source = source.replace("cancel: &AtomicBool", "cancel: &Arc<AtomicBool>");\n',
)

checker = CHECKER.read_text()
start_index = checker.index(HOOK_START)
end_index = checker.index(HOOK_END, start_index) + len(HOOK_END)
CHECKER.write_text(checker[:start_index] + checker[end_index:])
Path(__file__).unlink()
for marker in Path(".github").glob("team-observability-marker-*"):
    marker.unlink()
subprocess.run(["cargo", "fmt", "--all"], check=True)
