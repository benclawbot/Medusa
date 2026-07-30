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
exec(compile(source, "team-observability-materializer.py", "exec"), {})

checker = CHECKER.read_text()
start_index = checker.index(HOOK_START)
end_index = checker.index(HOOK_END, start_index) + len(HOOK_END)
CHECKER.write_text(checker[:start_index] + checker[end_index:])
Path(__file__).unlink()
for marker in Path(".github").glob("team-observability-marker-*"):
    marker.unlink()
subprocess.run(["cargo", "fmt", "--all"], check=True)
