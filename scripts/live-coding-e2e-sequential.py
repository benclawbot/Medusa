#!/usr/bin/env python3
"""Run the live coding harness with deterministic delegated-worker instructions."""

from __future__ import annotations

import importlib.util
import sys
import tempfile
from pathlib import Path


ORIGINAL_OBJECTIVE = '''        objective = (
            "Inspect this repository and repair all three product defects without modifying "
            "verify.py, test.mjs, package.json, fixtures, or expected outputs. Correct value.txt "
            "to the verified value, robustly implement src/slugify.py while preserving its public "
            "API, and repair the counter transitions in src/counter.js. Run `python verify.py`, "
            "iterate until every check passes, and stop only after all three independent "
            "validations succeed."
        )'''

DETERMINISTIC_OBJECTIVE = '''        objective = (
            "Repair exactly value.txt, src/slugify.py, and src/counter.js without modifying "
            "verify.py, test.mjs, package.json, fixtures, or expected outputs. Set value.txt to "
            "the verified value, robustly implement src/slugify.py while preserving its public "
            "API, and repair the counter transitions in src/counter.js. Run `python verify.py`, "
            "iterate until every check passes, and stop only after all three independent "
            "validations succeed. Any delegated read-only analysis or risk-review worker must "
            "inspect only the relevant files, send one concise evidence report to lead, and then "
            "immediately return a final text response. After team_send_message succeeds, do not "
            "call update_plan or any additional tool."
        )'''


def main() -> int:
    harness_path = Path(__file__).with_name("live-coding-e2e.py")
    source = harness_path.read_text(encoding="utf-8")
    if source.count(ORIGINAL_OBJECTIVE) != 1:
        raise RuntimeError("could not replace the live coding objective exactly once")
    source = source.replace(ORIGINAL_OBJECTIVE, DETERMINISTIC_OBJECTIVE)

    temporary_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            suffix=".py",
            prefix="live-coding-e2e-adjusted-",
            dir=harness_path.parent,
            delete=False,
        ) as temporary:
            temporary.write(source)
            temporary_path = Path(temporary.name)
        spec = importlib.util.spec_from_file_location(
            "medusa_live_coding_e2e", temporary_path
        )
        if spec is None or spec.loader is None:
            raise RuntimeError(f"could not load live coding harness from {temporary_path}")
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        spec.loader.exec_module(module)
        module.PARALLEL_WORKERS = 1
        return module.main()
    finally:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)


if __name__ == "__main__":
    raise SystemExit(main())
