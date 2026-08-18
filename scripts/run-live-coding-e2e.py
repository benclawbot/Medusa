#!/usr/bin/env python3
"""Run the existing coding dogfood harness with an explicit validated model selection."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def main() -> int:
    selector = load("live_model_selection", ROOT / "live-model-selection.py")
    harness = load("live_coding_e2e", ROOT / "live-coding-e2e.py")
    harness.MODEL = selector.selected_model()
    return int(harness.main())


if __name__ == "__main__":
    raise SystemExit(main())
