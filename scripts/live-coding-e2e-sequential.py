#!/usr/bin/env python3
"""Run the live coding harness as one bounded autonomous worker."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path


def main() -> int:
    harness_path = Path(__file__).with_name("live-coding-e2e.py")
    spec = importlib.util.spec_from_file_location("medusa_live_coding_e2e", harness_path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"could not load live coding harness from {harness_path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    module.PARALLEL_WORKERS = 1
    return module.main()


if __name__ == "__main__":
    raise SystemExit(main())
