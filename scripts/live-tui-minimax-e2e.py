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
EXPECTED_SOURCE = (
    f'pub const RESPONSE: &str = "{EXPECTED}";\n\n'
    "pub fn response() -> &'static str {\n    RESPONSE\n}\n\n"
    "#[cfg(test)]\nmod tests {\n    use super::*;\n\n"
    "    #[test]\n    fn response_is_not_empty() {\n"
    "        assert!(!response().is_empty());\n"
    "    }\n}\n"
)
PROMPT = (
    "Implement the requested repository change in src/lib.rs. The implementation task must "
    "use fs_write once on the relative path src/lib.rs and replace the complete file with the "
    "exact UTF-8 content between the markers below, including the final newline. Planning tasks "
    "should establish the exact scope, and review or verification tasks should evaluate the prepared "
    "implementation and authoritative evidence according to their assigned roles.\n<<<FILE\n"
    f"{EXPECTED_SOURCE}"
    ">>>FILE\nThe implementation task must not call shell_run, update_plan, team tools, or "
    "git_checkpoint. Medusa's authoritative host verifier will run formatting, build, lint, and tests "
    f"before integration. After fs_write succeeds, the implementation task must return exactly {EXPECTED}."
)
DEFAULT_TIMEOUT_SECONDS = 600
LIVE_MODEL = "MiniMax-M3"


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


def run(command: list[str], cwd: Path, timeout: int = 120) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        command,
        cwd=cwd,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=timeout,
    )


def write_profile(config_home: Path) -> None:
    medusa = config_home / "medusa"
    medusa.mkdir(parents=True, exist_ok=True)
    (medusa / "provider.toml").write_text(
        "\n".join(
            [
                'connection = "direct"',
                'provider = "minimax"',
                f'model = "{LIVE_MODEL}"',
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
    (repo / "src").mkdir(parents=True)
    (repo / ".medusa").mkdir()
    (repo / "Cargo.toml").write_text(
        '[package]\nname = "minimax-tui-fixture"\nversion = "0.1.0"\nedition = "2021"\n',
        encoding="utf-8",
    )
    (repo / "src" / "lib.rs").write_text(
        'pub const RESPONSE: &str = "TODO";\n\n'
        "pub fn response() -> &'static str {\n    RESPONSE\n}\n\n"
        "#[cfg(test)]\nmod tests {\n    use super::*;\n\n"
        "    #[test]\n    fn response_is_not_empty() {\n"
        "        assert!(!response().is_empty());\n"
        "    }\n}\n",
        encoding="utf-8",
    )
    # TUI RuntimeController::start loads repository configuration. Keeping the fixture limits here
    # proves the actual interactive path and prevents this acceptance task from reserving the
    # production default output budget for every orchestration role.
    (repo / ".medusa" / "config.toml").write_text(
        "[agent]\nmax_turns = 24\nparallel_workers = 1\n\n"
        "[model]\nmax_output_tokens = 2048\n",
        encoding="utf-8",
    )
    for command in (
        ["git", "init", "-q", "-b", "main"],
        ["git", "config", "user.name", "Medusa TUI E2E"],
        ["git", "config", "user.email", "medusa-tui-e2e@example.invalid"],
        ["cargo", "generate-lockfile"],
        ["git", "add", "Cargo.toml", "Cargo.lock", "src/lib.rs", ".medusa/config.toml"],
        ["git", "commit", "-q", "-m", "baseline"],
    ):
        result = run(command, repo)
        if result.returncode != 0:
            raise RuntimeError(result.stdout.decode("utf-8", errors="replace"))


def launch_tui(binary: Path, repo: Path, env: dict[str, str]) -> tuple[int, int]:
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(repo)
        os.execve(
            str(binary),
            [str(binary), "--repo", str(repo), "--prompt", PROMPT],
            env,
        )
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))
    os.set_blocking(fd, False)
    return pid, fd


def process_exited(pid: int) -> tuple[bool, int | None]:
    waited, status = os.waitpid(pid, os.WNOHANG)
    return (False, None) if waited == 0 else (True, os.waitstatus_to_exitcode(status))


def terminate(pid: int, fd: int) -> int | None:
    for payload in (b"\x03", b"\x03"):
        try:
            os.write(fd, payload)
            time.sleep(0.15)
        except OSError:
            break
    for sig in (signal.SIGTERM, signal.SIGKILL):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            exited, code = process_exited(pid)
            if exited:
                return code
            time.sleep(0.05)
        try:
            os.kill(pid, sig)
        except ProcessLookupError:
            break
    _, status = os.waitpid(pid, 0)
    return os.waitstatus_to_exitcode(status)


def assistant_contains(messages: object) -> bool:
    if not isinstance(messages, list):
        return False
    return any(
        isinstance(block, dict)
        and block.get("type") == "text"
        and EXPECTED in str(block.get("text", ""))
        for message in messages
        if isinstance(message, dict) and message.get("role") == "assistant"
        for block in (message.get("content") if isinstance(message.get("content"), list) else [])
    )


def session_evidence(repo: Path) -> tuple[list[Path], list[Path]]:
    responses: list[Path] = []
    assistants: list[Path] = []
    for path in (repo / ".medusa").rglob("*.json"):
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        if not isinstance(data, dict):
            continue
        events = data.get("events")
        if isinstance(events, list) and any(
            isinstance(event, dict)
            and isinstance(event.get("payload"), dict)
            and event["payload"].get("type")
            in {"model_response_received", "assistant_message_recorded", "session_completed"}
            for event in events
        ):
            responses.append(path)
        if assistant_contains(data.get("messages")):
            assistants.append(path)
    return responses, assistants


def durable_request_models(repo: Path) -> list[str]:
    models: set[str] = set()
    for path in (repo / ".medusa").rglob("*.json"):
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        if not isinstance(data, dict):
            continue
        events = data.get("events")
        if not isinstance(events, list):
            continue
        for event in events:
            if not isinstance(event, dict) or not isinstance(event.get("payload"), dict):
                continue
            payload = event["payload"]
            if payload.get("type") != "model_request_started" or payload.get("provider") != "minimax":
                continue
            model = payload.get("model")
            if isinstance(model, str) and model.strip():
                models.add(model.strip())
    return sorted(models)


def source_is_correct(repo: Path) -> bool:
    try:
        source = (repo / "src" / "lib.rs").read_text(encoding="utf-8")
    except OSError:
        return False
    return f'pub const RESPONSE: &str = "{EXPECTED}";' in source


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


def capture_evidence(repo: Path, output_dir: Path) -> tuple[bool, list[str]]:
    errors: list[str] = []
    tests_passed = False
    for name, command in (
        ("git-status.txt", ["git", "status", "--short"]),
        ("change.patch", ["git", "diff", "--binary"]),
        ("cargo-test.txt", ["cargo", "test", "--quiet", "--locked"]),
    ):
        try:
            result = run(command, repo)
            (output_dir / name).write_bytes(result.stdout)
            if name == "cargo-test.txt":
                tests_passed = result.returncode == 0
        except Exception as error:  # evidence collection must preserve the primary failure
            errors.append(f"{name}: {error}")
    try:
        copy_regular_tree(repo / ".medusa", output_dir / "medusa-state")
    except Exception as error:
        errors.append(f"medusa-state: {error}")
    return tests_passed, errors


def assert_secret_absent(roots: list[Path], secret: str) -> None:
    needle = secret.encode()
    for root in roots:
        if not root.exists():
            continue
        for path in root.rglob("*"):
            try:
                if stat.S_ISREG(path.lstat().st_mode) and needle in path.read_bytes():
                    raise RuntimeError(f"MiniMax credential was persisted in {path}")
            except OSError:
                continue


def main() -> int:
    args = parse_args()
    binary = args.binary.resolve()
    api_key = os.environ.get("MINIMAX_API_KEY", "")
    if args.timeout_seconds <= 0 or not binary.is_file() or not api_key:
        raise SystemExit("positive timeout, Medusa binary, and MINIMAX_API_KEY are required")

    work_root = args.work_root.resolve()
    output_dir = args.output_dir.resolve()
    shutil.rmtree(work_root, ignore_errors=True)
    shutil.rmtree(output_dir, ignore_errors=True)
    output_dir.mkdir(parents=True)
    repo, home = work_root / "repo", work_root / "home"
    home.mkdir(parents=True)
    initialize_repository(repo)
    write_profile(home / ".config")

    env = os.environ.copy()
    env.update(
        XDG_CONFIG_HOME=str(home / ".config"),
        XDG_CACHE_HOME=str(home / ".cache"),
        MINIMAX_API_KEY=api_key,
        PYTHONUTF8="1",
        TERM="xterm-256color",
    )

    started = time.monotonic()
    pid, fd = launch_tui(binary, repo, env)
    transcript = bytearray()
    submitted = rendered = False
    exit_code: int | None = None
    error: str | None = None
    change_offset: int | None = None
    try:
        while time.monotonic() - started < args.timeout_seconds:
            exited, code = process_exited(pid)
            if exited:
                exit_code = code
                break
            ready, _, _ = select.select([fd], [], [], 0.1)
            chunk_start = len(transcript)
            if ready:
                try:
                    transcript.extend(os.read(fd, 65536))
                except (BlockingIOError, OSError):
                    pass
            if not submitted and time.monotonic() - started >= 2:
                os.write(fd, b"\r")  # dismiss welcome
                time.sleep(0.2)
                os.write(fd, b"\r")  # submit initial prompt
                submitted = True

            text = transcript.decode("utf-8", errors="replace")
            if "Task failed" in text:
                error = "TUI rendered a task failure before a durable MiniMax completion"
                break
            if change_offset is None and source_is_correct(repo):
                change_offset = chunk_start
            responses, assistants = session_evidence(repo)
            if (
                submitted
                and change_offset is not None
                and EXPECTED in text
                and "Task completed" in text
                and responses
                and assistants
            ):
                rendered = True
                break
        if not rendered and error is None:
            error = (
                f"TUI exited before verified completion (exit={exit_code})"
                if exit_code is not None
                else f"TUI did not complete within {args.timeout_seconds}s"
            )
    finally:
        if exit_code is None:
            exit_code = terminate(pid, fd)
        try:
            os.close(fd)
        except OSError:
            pass

    text = transcript.decode("utf-8", errors="replace").replace(api_key, "[REDACTED]")
    (output_dir / "terminal.log").write_text(text, encoding="utf-8")
    responses, assistants = session_evidence(repo)
    request_models = durable_request_models(repo)
    model_matches_durable = not request_models or request_models == [LIVE_MODEL]
    source_correct = source_is_correct(repo)
    tests_passed, evidence_errors = capture_evidence(repo, output_dir)
    if not model_matches_durable:
        error = (
            f"selected model {LIVE_MODEL} does not match durable MiniMax request models: "
            + ", ".join(request_models)
        )
    passed = rendered and source_correct and tests_passed and error is None
    if rendered and not tests_passed:
        error = "MiniMax changed the file but cargo test did not pass"
        passed = False

    summary = {
        "schema_version": 4,
        "result": "pass" if passed else "fail",
        "provider": "minimax",
        "model": LIVE_MODEL,
        "durable_request_models": request_models,
        "model_matches_durable_evidence": model_matches_durable,
        "route": "saved-profile-to-interactive-tui",
        "fixture_max_output_tokens": 2048,
        "prompt_submitted": submitted,
        "post_change_marker_rendered": rendered,
        "assistant_marker_persisted": bool(assistants),
        "source_change_verified": source_correct,
        "cargo_test_passed": tests_passed,
        "durable_response_observed": bool(responses),
        "durable_response_files": [str(path.relative_to(repo)) for path in responses],
        "assistant_marker_files": [str(path.relative_to(repo)) for path in assistants],
        "elapsed_seconds": int(time.monotonic() - started),
        "exit_code": exit_code,
        "evidence_errors": evidence_errors,
        "error": error,
    }
    (output_dir / "summary.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    assert_secret_absent([home, repo, output_dir], api_key)
    print(json.dumps(summary, sort_keys=True))
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
