#!/usr/bin/env python3
"""Run the existing MiniMax TUI acceptance with the validated live model selection."""

from __future__ import annotations

import importlib.util
import sys
import time
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
    harness = load("live_tui_minimax_e2e", ROOT / "live-tui-minimax-e2e.py")
    model = selector.selected_model()

    def write_profile(config_home: Path) -> None:
        medusa = config_home / "medusa"
        medusa.mkdir(parents=True, exist_ok=True)
        (medusa / "provider.toml").write_text(
            "\n".join(
                [
                    'connection = "direct"',
                    'provider = "minimax"',
                    f'model = "{model}"',
                    'speed = "balanced"',
                    'reasoning = "medium"',
                    'auth = "api-key"',
                    "configured = true",
                    "",
                ]
            ),
            encoding="utf-8",
        )

    # The underlying real-TUI harness polls durable JSON evidence from the repository on every
    # terminal tick. Long sessions can accumulate enough state for that recursive scan to dominate
    # the loop and prevent its own wall-clock timeout from being observed. Keep the acceptance
    # semantics identical while bounding the expensive scan cadence on this validation-only branch.
    original_session_evidence = harness.session_evidence
    last_scan = 0.0
    cached_evidence: tuple[list[Path], list[Path]] = ([], [])

    def throttled_session_evidence(repo: Path) -> tuple[list[Path], list[Path]]:
        nonlocal last_scan, cached_evidence
        now = time.monotonic()
        if now - last_scan >= 5.0:
            cached_evidence = original_session_evidence(repo)
            last_scan = time.monotonic()
        return cached_evidence

    harness.write_profile = write_profile
    harness.session_evidence = throttled_session_evidence
    return int(harness.main())


if __name__ == "__main__":
    raise SystemExit(main())
