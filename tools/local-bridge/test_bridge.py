#!/usr/bin/env python3
from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from unittest import mock

import medusa_bridge


class BridgeTests(unittest.TestCase):
    def test_repo_root_requires_git_directory(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(ValueError):
                medusa_bridge.secure_repo_root(tmp)
            (Path(tmp) / ".git").mkdir()
            self.assertEqual(medusa_bridge.secure_repo_root(tmp), Path(tmp).resolve())

    def test_unknown_action_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / ".git").mkdir()
            with self.assertRaises(medusa_bridge.BridgeError) as ctx:
                medusa_bridge.run_action(
                    action_name="shell.run",
                    args=[],
                    repo_root=root,
                    allow_mutation=True,
                    timeout_seconds=10,
                    max_output_bytes=1024,
                )
            self.assertEqual(ctx.exception.status, medusa_bridge.HTTPStatus.NOT_FOUND)

    def test_mutation_requires_explicit_enablement(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / ".git").mkdir()
            with self.assertRaises(medusa_bridge.BridgeError) as ctx:
                medusa_bridge.run_action(
                    action_name="cargo.fmt",
                    args=[],
                    repo_root=root,
                    allow_mutation=False,
                    timeout_seconds=10,
                    max_output_bytes=1024,
                )
            self.assertEqual(ctx.exception.status, medusa_bridge.HTTPStatus.FORBIDDEN)

    def test_forbidden_git_config_argument_is_rejected(self) -> None:
        with self.assertRaises(medusa_bridge.BridgeError):
            medusa_bridge.validate_extra_args(["-c", "core.pager=cat"])
        with self.assertRaises(medusa_bridge.BridgeError):
            medusa_bridge.validate_extra_args(["--config=core.pager=cat"])

    @mock.patch("medusa_bridge.subprocess.run")
    def test_command_uses_argv_without_shell(self, run: mock.Mock) -> None:
        run.return_value = medusa_bridge.subprocess.CompletedProcess(
            ["git", "status", "--short", "--branch"], 0, b"ok\n", b""
        )
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / ".git").mkdir()
            result = medusa_bridge.run_action(
                action_name="git.status",
                args=[],
                repo_root=root,
                allow_mutation=False,
                timeout_seconds=10,
                max_output_bytes=1024,
            )
        self.assertTrue(result["success"])
        _, kwargs = run.call_args
        self.assertNotIn("shell", kwargs)
        self.assertEqual(kwargs["cwd"], root)
        self.assertIs(kwargs["stdin"], medusa_bridge.subprocess.DEVNULL)

    @mock.patch("medusa_bridge.subprocess.run")
    def test_output_is_truncated(self, run: mock.Mock) -> None:
        run.return_value = medusa_bridge.subprocess.CompletedProcess(
            ["git", "status", "--short", "--branch"], 0, b"x" * 2048, b""
        )
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / ".git").mkdir()
            result = medusa_bridge.run_action(
                action_name="git.status",
                args=[],
                repo_root=root,
                allow_mutation=False,
                timeout_seconds=10,
                max_output_bytes=1024,
            )
        self.assertEqual(len(result["stdout"]), 1024)
        self.assertTrue(result["stdout_truncated"])


if __name__ == "__main__":
    unittest.main()
