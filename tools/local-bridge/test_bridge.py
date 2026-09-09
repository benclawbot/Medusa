#!/usr/bin/env python3
from __future__ import annotations

import io
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

import medusa_bridge


class BridgeTests(unittest.TestCase):
    class FakeProcess:
        pid = 1234

        def __init__(self, stdout: bytes = b"", stderr: bytes = b"", returncode: int = 0) -> None:
            self.stdout = io.BytesIO(stdout)
            self.stderr = io.BytesIO(stderr)
            self.returncode = returncode

        def wait(self, timeout: float | None = None) -> int:
            return self.returncode

        def poll(self) -> int:
            return self.returncode

    def test_repo_root_requires_git_directory(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(ValueError):
                medusa_bridge.secure_repo_root(tmp)
            (Path(tmp) / ".git").mkdir()
            self.assertEqual(medusa_bridge.secure_repo_root(tmp), Path(tmp).resolve())

    def test_bind_host_accepts_loopback_values(self) -> None:
        self.assertEqual(medusa_bridge.validate_bind_host("127.0.0.1"), "127.0.0.1")
        self.assertEqual(medusa_bridge.validate_bind_host("localhost"), "localhost")
        self.assertEqual(medusa_bridge.validate_bind_host("LOCALHOST"), "LOCALHOST")
        self.assertEqual(medusa_bridge.validate_bind_host("::1"), "::1")
        self.assertEqual(medusa_bridge.validate_bind_host("[::1]"), "::1")

    def test_server_type_matches_loopback_address_family(self) -> None:
        self.assertIs(
            medusa_bridge.server_type_for_host("127.0.0.1"),
            medusa_bridge.ThreadingHTTPServer,
        )
        self.assertIs(
            medusa_bridge.server_type_for_host("localhost"),
            medusa_bridge.ThreadingHTTPServer,
        )
        self.assertIs(
            medusa_bridge.server_type_for_host("::1"),
            medusa_bridge.IPv6ThreadingHTTPServer,
        )

    def test_bind_host_rejects_non_loopback_values(self) -> None:
        for host in ("0.0.0.0", "192.168.1.10", "8.8.8.8", "example.com", ""):
            with self.subTest(host=host), self.assertRaises(ValueError):
                medusa_bridge.validate_bind_host(host)

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

    def test_read_only_actions_reject_output_and_executable_overrides(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / ".git").mkdir()
            outside = root.parent / "bridge-output.txt"
            try:
                for args in (
                    ["--output", str(outside)],
                    [f"--output={outside}"],
                    ["-o", str(outside)],
                    [f"-o{outside}"],
                    ["--exec=fixture"],
                    ["--upload-pack=fixture"],
                    ["-xfixture"],
                    ["-c", "core.pager=cat"],
                ):
                    with self.subTest(args=args), self.assertRaises(medusa_bridge.BridgeError):
                        medusa_bridge.run_action(
                            action_name="git.diff",
                            args=args,
                            repo_root=root,
                            allow_mutation=False,
                            timeout_seconds=10,
                            max_output_bytes=1024,
                        )
                self.assertFalse(outside.exists())
            finally:
                outside.unlink(missing_ok=True)

    def test_read_only_diff_accepts_a_repository_relative_filter(self) -> None:
        with mock.patch("medusa_bridge.subprocess.Popen") as popen:
            popen.return_value = self.FakeProcess()
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                (root / ".git").mkdir()
                result = medusa_bridge.run_action(
                    action_name="git.diff",
                    args=["--", "src/*.rs"],
                    repo_root=root,
                    allow_mutation=False,
                    timeout_seconds=10,
                    max_output_bytes=1024,
                )
            self.assertTrue(result["success"])
            self.assertEqual(popen.call_args.args[0], ["git", "diff", "--stat", "--", "src/*.rs"])

    def test_timeout_terminates_descendants_and_preserves_bounded_output(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / ".git").mkdir()
            marker = root / "late-marker"
            fixture = root / "timeout_fixture.py"
            fixture.write_text(
                "import pathlib, subprocess, sys, time\n"
                "marker = pathlib.Path(sys.argv[1])\n"
                "subprocess.Popen([sys.executable, '-c', "
                "'import pathlib,sys,time; time.sleep(2); pathlib.Path(sys.argv[1]).write_text(\\\"late\\\")', str(marker)])\n"
                "sys.stdout.write('x' * (4 * 1024 * 1024))\n"
                "sys.stderr.write('y' * (4 * 1024 * 1024))\n"
                "sys.stdout.flush(); sys.stderr.flush(); time.sleep(10)\n",
                encoding="utf-8",
            )
            action_name = "test.timeout"
            medusa_bridge.ACTIONS[action_name] = medusa_bridge.Action(
                (sys.executable, str(fixture), str(marker))
            )
            try:
                result = medusa_bridge.run_action(
                    action_name=action_name,
                    args=[],
                    repo_root=root,
                    allow_mutation=False,
                    timeout_seconds=1,
                    max_output_bytes=1024,
                )
            finally:
                del medusa_bridge.ACTIONS[action_name]
            self.assertTrue(result["timed_out"])
            self.assertTrue(result["stdout_truncated"])
            self.assertTrue(result["stderr_truncated"])
            self.assertLessEqual(len(result["stdout"].encode()), 1024)
            self.assertLessEqual(len(result["stderr"].encode()), 1024)
            time.sleep(1.4)
            self.assertFalse(marker.exists(), "timeout left a descendant alive")

    @mock.patch("medusa_bridge.subprocess.Popen")
    def test_command_uses_argv_without_shell(self, popen: mock.Mock) -> None:
        popen.return_value = self.FakeProcess(stdout=b"ok\n")
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
        _, kwargs = popen.call_args
        self.assertNotIn("shell", kwargs)
        self.assertEqual(kwargs["cwd"], root)
        self.assertIs(kwargs["stdin"], medusa_bridge.subprocess.DEVNULL)
        self.assertTrue(kwargs["start_new_session"])

    @mock.patch("medusa_bridge.subprocess.Popen")
    def test_output_is_truncated(self, popen: mock.Mock) -> None:
        popen.return_value = self.FakeProcess(stdout=b"x" * 2048)
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
