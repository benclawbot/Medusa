#!/usr/bin/env python3
"""Copy the validated team-observability source into a clean validation branch."""

from __future__ import annotations

import subprocess
import urllib.request
from pathlib import Path

BRANCH = "agent/team-observability-steering"
RAW = f"https://raw.githubusercontent.com/benclawbot/Medusa/{BRANCH}"
CHECKER = Path("scripts/check-product-architecture.py")
HOOK_START = "# BEGIN TEAM OBSERVABILITY MATERIALIZER\n"
HOOK_END = "# END TEAM OBSERVABILITY MATERIALIZER\n"
FILES = [
    "apps/medusa-desktop/src-tauri/src/dto.rs",
    "apps/medusa-desktop/src/runtime.timeline.test.ts",
    "apps/medusa-desktop/src/runtime.ts",
    "crates/medusa-runtime/build_main.rs",
    "crates/medusa-runtime/src/commands.rs",
    "crates/medusa-runtime/src/error.rs",
    "crates/medusa-runtime/src/multi_agent_coordinator.rs",
    "crates/medusa-runtime/src/mutating_worker_coordinator.rs",
    "crates/medusa-runtime/src/mutating_worker_failure.rs",
    "crates/medusa-runtime/src/runtime_impl.rs",
    "crates/medusa-runtime/src/team_control.rs",
    "crates/medusa-tui/src/runtime.rs",
    "crates/medusa-tui/src/session.rs",
    "docs/TEAM-OBSERVABILITY-STEERING.md",
]


def download(path: str) -> str:
    with urllib.request.urlopen(f"{RAW}/{path}", timeout=30) as response:
        return response.read().decode("utf-8")


for path in FILES:
    target = Path(path)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(download(path))

mutating = Path("crates/medusa-runtime/src/mutating_worker_coordinator.rs")
source = mutating.read_text()
for anchor in ("pub fn run_implementation(", "fn integrate_prepared("):
    count = source.count(anchor)
    if count != 1:
        raise SystemExit(f"expected one {anchor!r}, found {count}")
    source = source.replace(anchor, f"#[allow(clippy::too_many_arguments)]\n{anchor}", 1)
mutating.write_text(source)

checker = CHECKER.read_text()
start = checker.index(HOOK_START)
end = checker.index(HOOK_END, start) + len(HOOK_END)
CHECKER.write_text(checker[:start] + checker[end:])
Path(__file__).unlink()
for marker in Path(".github").glob("team-observability-marker-*"):
    marker.unlink()
subprocess.run(["cargo", "fmt", "--all"], check=True)
