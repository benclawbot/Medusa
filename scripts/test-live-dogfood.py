#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import json
import tempfile
import sys
import unittest
from pathlib import Path


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


ROOT = Path(__file__).resolve().parent
HARNESS = load("live_coding_e2e", ROOT / "live-coding-e2e.py")
REPORT = load("live_dogfood_report", ROOT / "live-dogfood-report.py")


class LiveDogfoodContractTests(unittest.TestCase):
    def test_configure_utf8_stdio_handles_windows_console_encoding(self) -> None:
        class FakeStream:
            def __init__(self) -> None:
                self.calls: list[dict[str, str]] = []

            def reconfigure(self, **kwargs: str) -> None:
                self.calls.append(kwargs)

        original_stdout, original_stderr = sys.stdout, sys.stderr
        fake_stdout, fake_stderr = FakeStream(), FakeStream()
        try:
            HARNESS.sys.stdout = fake_stdout
            HARNESS.sys.stderr = fake_stderr
            HARNESS.configure_utf8_stdio()
        finally:
            HARNESS.sys.stdout = original_stdout
            HARNESS.sys.stderr = original_stderr

        self.assertEqual(fake_stdout.calls, [{"encoding": "utf-8", "errors": "replace"}])
        self.assertEqual(fake_stderr.calls, [{"encoding": "utf-8", "errors": "replace"}])

    def test_sanitizer_removes_exact_credentials(self) -> None:
        self.assertEqual(
            HARNESS.sanitize("prefix secret-value suffix", ["secret-value"]),
            "prefix [REDACTED] suffix",
        )

    def test_failure_classifier_distinguishes_provider_and_product(self) -> None:
        self.assertEqual(HARNESS.classify_failure("HTTP 429 rate limit"), "provider")
        self.assertEqual(HARNESS.classify_failure("verification assertion failed"), "product")

    def test_secret_persistence_audit_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            harness = HARNESS.Harness(
                repo_root=root,
                output_dir=root / "artifacts",
                timeout_seconds=1,
                heartbeat_seconds=1,
                api_key="credential-value",
            )
            harness.fixture.mkdir(parents=True)
            (harness.fixture / "leak.txt").write_text("credential-value", encoding="utf-8")
            with self.assertRaises(HARNESS.HarnessError):
                harness.assert_secret_not_persisted()

    def test_report_requires_three_platforms_on_one_commit(self) -> None:
        summaries = []
        for platform in sorted(REPORT.EXPECTED_PLATFORMS):
            summaries.append(
                {
                    "schema_version": 1,
                    "result": "passed",
                    "classification": None,
                    "detail": None,
                    "commit": "abc123",
                    "platform": platform,
                    "provider": "minimax",
                    "model": "MiniMax-M3",
                    "passed": 3,
                    "build": {
                        "binary_sha256": "a" * 64,
                        "architecture": "fixture",
                        "os_release": "fixture",
                    },
                    "usage": {
                        "model_turns": 4,
                        "total_tokens": 1000,
                        "estimated_cost_microusd": 100,
                    },
                    "total": 3,
                    "credential_persisted": False,
                    "verification_contract_unchanged": True,
                    "bounded": {
                        "timeout_seconds": 1500,
                        "max_turns": 16,
                        "parallel_workers": 2,
                        "max_output_tokens": 4096,
                        "context_window_tokens": 32768,
                        "max_retries": 2,
                        "max_cost_microusd": 20_000_000,
                    },
                }
            )
        self.assertEqual(REPORT.validate(summaries), [])
        summaries.pop()
        self.assertTrue(REPORT.validate(summaries))

    def test_failed_summary_does_not_require_a_built_binary(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            harness = HARNESS.Harness(
                repo_root=root,
                output_dir=root / "artifacts",
                timeout_seconds=1,
                heartbeat_seconds=1,
                api_key="credential-value",
            )
            harness.commit_sha = lambda: "abc123"
            harness.write_summary(
                result="failed",
                classification="environment",
                detail="release build failed",
            )
            summary = json.loads(
                (harness.output_dir / "summary.json").read_text(encoding="utf-8")
            )
            self.assertEqual(summary["result"], "failed")
            self.assertEqual(summary["commit"], "abc123")
            self.assertIsNone(summary["build"]["binary_sha256"])

    def test_report_loader_ignores_unrelated_json(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "summary.json").write_text(json.dumps({"result": "other"}), encoding="utf-8")
            self.assertEqual(REPORT.load_summaries(root), [])


if __name__ == "__main__":
    unittest.main()
