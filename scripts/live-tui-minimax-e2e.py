#!/usr/bin/env python3
"""Bounded live MiniMax acceptance through the real interactive Medusa TUI."""

from __future__ import annotations

import argparse
import fcntl
import json
import os
import pty
import select
import shutil
import signal
import stat
import struct
import subprocess
import termios
import time
from pathlib import Path

EXPECTED = "MEDUSA_TUI_MINIMAX_OK"
PROMPT = (
    "Modify src/lib.rs so the public constant RESPONSE has the exact value "
    f"{EXPECTED}. Keep the existing test unchanged. Run cargo test to verify the change. "
    f"When the task is complete, finish your final response with exactly {EXPECTED}."
)
DEFAULT_TIMEOUT_SECONDS = 600


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--work-root", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument(
        "--timeout-seconds",
        type=int,
        default=int(os.environ.get("LIVE_TUI_TIMEOUT_SECONDS", DEFAULT_TIMEOUT_SECONDS)),
    )
    return parser.parse_args()


def configure_terminal(fd: int, rows: int = 40, columns: int = 140) -> None:
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))


def sanitize(value: str, secret: str) -> str:
    return value.replace(secret, "[REDACTED]") if secret else value


def write_profile(config_home: Path) -> None:
    medusa = config_home / "medusa"
    medusa.mkdir(parents=True, exist_ok=True)
    (medusa / "provider.toml").write_text(
        "\n".join(
            [
                'connection = "direct"',
                'provider = "minimax"',
                'model = "MiniMax-M3"',
                'speed = "balanced"',
                'reasoning = "medium"',
                'auth = "api-key"',
                "configured = true",
                "",
            ]
        ),
        encoding="utf-8",
    )


def initialize_repository(repo: Path) -> None:
    repo.mkdir(parents=True, exist_ok=True)
    (repo / "src").mkdir()
    (repo / "Cargo.toml").write_text(
        "[package]\nname = \"minimax-tui-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        encoding="utf-8",
    )
    (repo / "src" / "lib.rs").write_text(
        "pub const RESPONSE: &str = \"TODO\";\n\n"
        "pub fn response() -> &'static str {\n    RESPONSE\n}\n\n"
        "#[cfg(test)]\nmod tests {\n    use super::*;\n\n"
        "    #[test]\n    fn response_is_not_empty() {\n"
        "        assert!(!response().is_empty());\n"
        "    }\n}\n",
        encoding="utf-8",
    )
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo, check=True)
    subprocess.run(["git", "config", "user.name", "Medusa TUI E2E"], cwd=repo, check=True)
    subprocess.run(
        ["git", "config", "user.email", "medusa-tui-e2e@example.invalid"],
        cwd=repo,
        check=True,
    )
    subprocess.run(["git", "add", "Cargo.toml", "src/lib.rs"], cwd=repo, check=True)
    subprocess.run(["git", "commit", "-q", "-m", "baseline"], cwd=repo, check=True)


def launch_tui(binary: Path, repo: Path, env: dict[str, str]) -> tuple[int, int]:
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(repo)
        argv = [
            str(binary),
            "--repo",
            str(repo),
            "--set",
            "agent.max_turns=8",
            "--set",
            "agent.parallel_workers=1",
            "--prompt",
            PROMPT,
        ]
        os.execve(str(binary), argv, env)
    configure_terminal(fd)
    os.set_blocking(fd, False)
    return pid, fd


def process_exited(pid: int) -> tuple[bool, int | None]:
    waited, status = os.waitpid(pid, os.WNOHANG)
    if waited == 0:
        return False, None
    return True, os.waitstatus_to_exitcode(status)


def terminate(pid: int, fd: int) -> int | None:
    try:
        os.write(fd, b"\x03")
        time.sleep(0.15)
        os.write(fd, b"\x03")
    except OSError:
        pass
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        exited, code = process_exited(pid)
        if exited:
            return code
        time.sleep(0.05)
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        exited, code = process_exited(pid)
        if exited:
            return code
        time.sleep(0.05)
    try:
        os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    _, status = os.waitpid(pid, 0)
    return os.waitstatus_to_exitcode(status)


def assistant_text_contains(messages: object, marker: str) -> bool:
    if not isinstance(messages, list):
        return False
    for message in messages:
        if not isinstance(message, dict) or message.get("role") != "assistant":
            continue
        content = message.get("content", [])
        if not isinstance(content, list):
            continue
        for block in content:
            if (
                isinstance(block, dict)
                and block.get("type") == "text"
                and marker in str(block.get("text", ""))
            ):
                return True
    return False


def session_evidence(repo: Path) -> tuple[list[Path], list[Path]]:
    medusa = repo / ".medusa"
    if not medusa.is_dir():
        return [], []
    response_paths: list[Path] = []
    assistant_marker_paths: list[Path] = []
    for path in medusa.rglob("*.json"):
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        if not isinstance(data, dict):
            continue
        events = data.get("events", [])
        response_seen = isinstance(events, list) and any(
            isinstance(event, dict)
            and isinstance(event.get("payload"), dict)
            and event["payload"].get("type") == "model_response_received"
            for event in events
        )
        if response_seen:
            response_paths.append(path)
        if assistant_text_contains(data.get("messages"), EXPECTED):
            assistant_marker_paths.append(path)
    return response_paths, assistant_marker_paths


def source_is_correct(repo: Path) -> bool:
    try:
        source = (repo / "src" / "lib.rs").read_text(encoding="utf-8")
    except OSError:
        return False
    return f'pub const RESPONSE: &str = "{EXPECTED}";' in source


def cargo_test_passes(repo: Path) -> bool:
    result = subprocess.run(
        ["cargo", "test", "--quiet"],
        cwd=repo,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=120,
    )
    return result.returncode == 0


def copy_regular_tree(source: Path, destination: Path) -> None:
    for path in source.rglob("*"):
        try:
            mode = path.lstat().st_mode
        except OSError:
            continue
        if not stat.S_ISREG(mode):
            continue
        target = destination / path.relative_to(source)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, target)


def capture_repository_evidence(repo: Path, output_dir: Path) -> list[str]:
    errors: list[str] = []
    for name, command in (
        ("git-status.txt", ["git", "status", "--short"]),
        ("change.patch", ["git", "diff", "--binary"]),
        ("cargo-test.txt", ["cargo", "test", "--quiet"]),
    ):
        try:
            result = subprocess.run(
                command,
                cwd=repo,
                check=False,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                timeout=120,
            )
            (output_dir / name).write_text(result.stdout, encoding="utf-8")
        except Exception as error:
            errors.append(f"{name}: {error}")
    medusa = repo / ".medusa"
    if medusa.is_dir():
        try:
            copy_regular_tree(medusa, output_dir / "medusa-state")
        except Exception as error:
            errors.append(f"medusa-state: {error}")
    return errors


def assert_secret_not_persisted(roots: list[Path], secret: str) -> None:
    needle = secret.encode("utf-8")
    for root in roots:
        if not root.exists():
            continue
        for path in root.rglob("*"):
            try:
                mode = path.lstat().st_mode
            except OSError:
                continue
            if stat.S_ISREG(mode) and needle in path.read_bytes():
                raise RuntimeError(f"MiniMax credential was persisted in {path}")


def main() -> int:
    args = parse_args()
    if args.timeout_seconds <= 0:
        raise SystemExit("timeout must be positive")
    binary = args.binary.resolve()
    if not binary.is_file():
        raise SystemExit(f"Medusa binary not found: {binary}")
    api_key = os.environ.get("MINIMAX_API_KEY", "")
    if not api_key:
        raise SystemExit("MINIMAX_API_KEY is required")

    work_root = args.work_root.resolve()
    output_dir = args.output_dir.resolve()
    shutil.rmtree(work_root, ignore_errors=True)
    shutil.rmtree(output_dir, ignore_errors=True)
    output_dir.mkdir(parents=True, exist_ok=True)
    repo = work_root / "repo"
    home = work_root / "home"
    config_home = home / ".config"
    home.mkdir(parents=True, exist_ok=True)
    initialize_repository(repo)
    write_profile(config_home)

    env = os.environ.copy()
    env.update(
        {
            "HOME": str(home),
            "XDG_CONFIG_HOME": str(config_home),
            "XDG_CACHE_HOME": str(home / ".cache"),
            "MINIMAX_API_KEY": api_key,
            "PYTHONUTF8": "1",
            "TERM": "xterm-256color",
        }
    )

    started = time.monotonic()
    pid, fd = launch_tui(binary, repo, env)
    transcript = bytearray()
    submitted = False
    rendered = False
    exit_code: int | None = None
    error: str | None = None
    source_change_offset: int | None = None
    latest_chunk_start = 0
    try:
        while time.monotonic() - started < args.timeout_seconds:
            exited, code = process_exited(pid)
            if exited:
                exit_code = code
                break
            ready, _, _ = select.select([fd], [], [], 0.1)
            if ready:
                try:
                    latest_chunk_start = len(transcript)
                    transcript.extend(os.read(fd, 65536))
                except BlockingIOError:
                    pass
                except OSError:
                    break
            elapsed = time.monotonic() - started
            if not submitted and elapsed >= 2:
                os.write(fd, b"\r")
                time.sleep(0.2)
                os.write(fd, b"\r")
                submitted = True

            text = transcript.decode("utf-8", errors="replace")
            if "Task failed" in text:
                error = "TUI rendered a task failure before a durable MiniMax completion"
                break
            if source_change_offset is None and source_is_correct(repo):
                source_change_offset = latest_chunk_start

            response_paths, assistant_paths = session_evidence(repo)
            post_change_text = (
                transcript[source_change_offset:].decode("utf-8", errors="replace")
                if source_change_offset is not None
                else ""
            )
            if (
                submitted
                and source_change_offset is not None
                and EXPECTED in post_change_text
                and response_paths
                and assistant_paths
            ):
                rendered = True
                break
        if not rendered and error is None:
            if exit_code is not None:
                error = f"TUI exited before rendering the MiniMax response (exit={exit_code})"
            else:
                error = f"TUI did not complete the verified MiniMax task within {args.timeout_seconds}s"
    finally:
        if exit_code is None:
            exit_code = terminate(pid, fd)
        try:
            os.close(fd)
        except OSError:
            pass

    text = sanitize(transcript.decode("utf-8", errors="replace"), api_key)
    (output_dir / "terminal.log").write_text(text, encoding="utf-8")
    response_paths, assistant_paths = session_evidence(repo)
    source_correct = source_is_correct(repo)
    tests_passed = cargo_test_passes(repo) if source_correct else False
    evidence_errors = capture_repository_evidence(repo, output_dir)
    if rendered and not tests_passed:
        rendered = False
        error = "MiniMax changed the file but cargo test did not pass"

    summary = {
        "schema_version": 3,
        "result": "pass" if rendered and error is None and tests_passed else "fail",
        "provider": "minimax",
        "model": "MiniMax-M3",
        "route": "saved-profile-to-interactive-tui",
        "prompt_submitted": submitted,
        "post_change_marker_rendered": rendered,
        "assistant_marker_persisted": bool(assistant_paths),
        "source_change_verified": source_correct,
        "cargo_test_passed": tests_passed,
        "durable_response_observed": bool(response_paths),
        "durable_response_files": [str(path.relative_to(repo)) for path in response_paths],
        "assistant_marker_files": [str(path.relative_to(repo)) for path in assistant_paths],
        "elapsed_seconds": int(time.monotonic() - started),
        "exit_code": exit_code,
        "evidence_errors": evidence_errors,
        "error": error,
    }
    (output_dir / "summary.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )

    assert_secret_not_persisted([home, repo, output_dir], api_key)
    print(json.dumps(summary, sort_keys=True))
    return 0 if summary["result"] == "pass" else 1


if __name__ == "__main__":
    raise SystemExit(main())
