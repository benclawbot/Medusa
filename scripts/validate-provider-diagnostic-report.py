#!/usr/bin/env python3
"""Validate deterministic provider diagnostic evidence without leaking credential names."""

from __future__ import annotations

import argparse
import json
from pathlib import Path

SENSITIVE_ENV_NAMES = (
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "MINIMAX_API_KEY",
    "MEDUSA_API_KEY",
)


def validate(path: Path) -> None:
    report = json.loads(path.read_text(encoding="utf-8"))
    assert report["schema_version"] == 1
    assert report["status"] == "ready"
    assert report["provider"] == "local"
    assert report["streaming"]["supported"] is True
    assert report["image_input"]["supported"] is False
    assert report["failures"] == []
    serialized = json.dumps(report)
    for secret_name in SENSITIVE_ENV_NAMES:
        assert secret_name not in serialized


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("report", type=Path)
    args = parser.parse_args()
    validate(args.report)
    print("provider-diagnostic-report-ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
