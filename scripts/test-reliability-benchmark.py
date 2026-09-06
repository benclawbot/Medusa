#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import json
import re
import subprocess
import tempfile
from pathlib import Path

SCRIPT = Path(__file__).with_name("reliability-benchmark.py")
SPEC = importlib.util.spec_from_file_location("reliability_benchmark", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def summary(status: str = "passed") -> dict:
    ids = [
        "production-orchestration",
        "headless-entrypoint",
        "verification-rollback",
        "upgrade-rollback-evidence",
        "interruption-resume",
        "checkpoint-restore",
        "filesystem-network-process-boundary",
    ]
    return {
        "schema_version": 1,
        "metrics": {"false_completion_rate": 0.0, "manual_interventions": 0},
        "scenarios": [
            {"id": item, "status": status, "duration_ms": 10, "log": f"{item}.log"}
            for item in ids
        ],
    }


def main() -> int:
    workflow = Path(".github/workflows/reliability-benchmarks.yml").read_text(encoding="utf-8")
    deterministic_job = re.search(
        r"(?ms)^  deterministic-runtime:\n.*?^    timeout-minutes: (\d+)$", workflow
    )
    assert deterministic_job and int(deterministic_job.group(1)) == 60

    suite = json.loads(Path("benchmarks/reliability-suite.json").read_text(encoding="utf-8"))
    assert MODULE.DEFAULT_ACCEPTANCE_TIMEOUT_SECONDS == 600
    report = MODULE.score(suite, [summary(), summary()])
    assert report["passed"]
    assert report["metrics"]["verified_completion_rate"] == 1.0
    assert report["metrics"]["false_completion_rate"] == 0.0
    assert report["metrics"]["repeated_run_determinism"] == 1.0

    empty = MODULE.score(suite, [])
    assert not empty["passed"]
    assert empty["metrics"]["verified_completion_rate"] is None

    unmeasured = summary()
    unmeasured.pop("metrics")
    report = MODULE.score(suite, [unmeasured, summary()])
    assert not report["passed"]
    assert any(item["metric"] == "false_completion_rate" for item in report["failures"])
    assert report["metrics"]["false_completion_rate"] is None

    failed = summary()
    failed["scenarios"][0]["status"] = "failed"
    report = MODULE.score(suite, [summary(), failed])
    assert not report["passed"]
    assert any(item["metric"] == "verified_completion_rate" for item in report["failures"])

    changed = summary()
    changed["scenarios"].append(
        {"id": "unexpected", "status": "passed", "duration_ms": 1, "log": "unexpected.log"}
    )
    report = MODULE.score(suite, [summary(), changed])
    assert report["metrics"]["repeated_run_determinism"] == 0.0

    with tempfile.TemporaryDirectory() as directory:
        output = Path(directory)
        (output / "report.md").write_text(MODULE.markdown(report), encoding="utf-8")
        assert "Reliability and recovery benchmark results" in (output / "report.md").read_text()

    with tempfile.TemporaryDirectory() as directory:
        output = Path(directory)
        (output / "summary.json").write_text(json.dumps(summary()), encoding="utf-8")
        original_run = MODULE.subprocess.run
        calls = []
        previous_timeout = MODULE.os.environ.pop("MEDUSA_ACCEPTANCE_TIMEOUT_SECONDS", None)
        try:
            MODULE.subprocess.run = lambda *args, **kwargs: (
                calls.append(kwargs),
                subprocess.CompletedProcess(args[0], 17),
            )[1]
            try:
                MODULE.run_acceptance(output, 1, 1)
            except RuntimeError as error:
                assert "exit status 17" in str(error)
            else:
                raise AssertionError("failed product acceptance was accepted")
        finally:
            MODULE.subprocess.run = original_run
            if previous_timeout is not None:
                MODULE.os.environ["MEDUSA_ACCEPTANCE_TIMEOUT_SECONDS"] = previous_timeout
        assert calls[0]["timeout"] == MODULE.DEFAULT_ACCEPTANCE_TIMEOUT_SECONDS

    print("reliability-benchmark-fixtures-ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
