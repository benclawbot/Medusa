#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


SELECTOR = load("live_model_selection_test", ROOT / "live-model-selection.py")
REPORT = load("live_dogfood_report_runner_test", ROOT / "run-live-dogfood-report.py")


class LiveModelSelectionTests(unittest.TestCase):
    def test_default_preserves_canonical_primary_model(self) -> None:
        self.assertEqual(SELECTOR.selected_model({}), "MiniMax-M3")

    def test_explicit_m27_override_is_allowed(self) -> None:
        self.assertEqual(
            SELECTOR.selected_model({SELECTOR.OVERRIDE_ENV: "MiniMax-M2.7"}),
            "MiniMax-M2.7",
        )

    def test_unknown_override_fails_closed(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "unsupported MEDUSA_LIVE_MODEL"):
            SELECTOR.selected_model({SELECTOR.OVERRIDE_ENV: "typo-model"})

    def test_report_requires_one_actual_model(self) -> None:
        provider, model, errors = REPORT.evidence_route(
            [
                {"provider": "minimax", "model": "MiniMax-M2.7"},
                {"provider": "minimax", "model": "MiniMax-M2.7"},
            ]
        )
        self.assertEqual((provider, model), ("minimax", "MiniMax-M2.7"))
        self.assertEqual(errors, [])

    def test_report_rejects_mixed_models(self) -> None:
        _, _, errors = REPORT.evidence_route(
            [
                {"provider": "minimax", "model": "MiniMax-M3"},
                {"provider": "minimax", "model": "MiniMax-M2.7"},
            ]
        )
        self.assertTrue(errors)


if __name__ == "__main__":
    unittest.main()
