#!/usr/bin/env python3
"""Medusa local execution bridge.

A small, dependency-free localhost service that exposes a constrained set of
repository maintenance commands to an orchestrator. It never invokes a shell,
binds only to loopback, requires a bearer token, confines execution to a
configured repository, and writes an append-only JSONL audit log.
"""

from __future__ import annotations

import argparse
import hmac
import ipaddress
import json
import os
import secrets
import signal
import socket
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, Final

MAX_BODY_BYTES: Final = 64 * 1024
DEFAULT_MAX_OUTPUT_BYTES: Final = 2 * 1024 * 1024
DEFAULT_TIMEOUT_SECONDS: Final = 900


@dataclass(frozen=True)
class Action:
    argv: tuple[str, ...]
    mutating: bool = False
    allow_extra: bool = False
    argument_policy: str = "none"


ACTIONS: Final[dict[str, Action]] = {
    "cargo.fmt": Action(("cargo", "fmt", "--all"), mutating=True),
    "cargo.fmt-check": Action(("cargo", "fmt", "--all", "--", "--check")),
    "cargo.generate-lockfile": Action(("cargo", "generate-lockfile"), mutating=True),
    "cargo.check": Action(
        ("cargo", "check", "--workspace", "--all-targets"),
        allow_extra=True,
        argument_policy="cargo",
    ),
    "cargo.clippy": Action(
        ("cargo", "clippy", "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings"),
        allow_extra=False,
    ),
    "cargo.test": Action(
        ("cargo", "test", "--workspace", "--all-features"),
        allow_extra=True,
        argument_policy="cargo",
    ),
    "cargo.doc": Action(
        ("cargo", "doc", "--workspace", "--no-deps"),
        allow_extra=True,
        argument_policy="cargo",
    ),
    "git.status": Action(("git", "status", "--short", "--branch")),
    "git.diff": Action(("git", "diff", "--stat"), allow_extra=True, argument_policy="git.diff"),
    "git.diff-check": Action(("git", "diff", "--check")),
    "git.add": Action(("git", "add", "--"), mutating=True, allow_extra=True, argument_policy="git.paths"),
    "git.commit": Action(("git", "commit"), mutating=True, allow_extra=True, argument_policy="git.commit"),
    "git.push": Action(("git", "push"), mutating=True, allow_extra=True, argument_policy="git.generic"),
    "git.fetch": Action(("git", "fetch", "--prune"), mutating=True, allow_extra=True, argument_policy="git.generic"),
    "git.checkout": Action(("git", "checkout"), mutating=True, allow_extra=True, argument_policy="git.generic"),
    "git.rebase": Action(("git", "rebase"), mutating=True, allow_extra=True, argument_policy="git.generic"),
    "gh.auth-status": Action(("gh", "auth", "status")),
    "gh.pr-checks": Action(("gh", "pr", "checks"), allow_extra=True, argument_policy="gh"),
    "gh.run-view": Action(("gh", "run", "view"), allow_extra=True, argument_policy="gh"),
}

MAX_ARGUMENTS: Final = 64
MAX_ARGUMENT_BYTES: Final = 8192
PROCESS_READ_CHUNK_BYTES: Final = 64 * 1024
PROCESS_TERMINATION_GRACE_SECONDS: Final = 2

EXECUTABLE_OVERRIDE_OPTIONS: Final = {
    "--config",
    "--config-env",
    "--exec",
    "--receive-pack",
    "--upload-pack",
}
OUTPUT_OVERRIDE_OPTIONS: Final = {
    "--artifact-dir",
    "--build-dir",
    "--out-dir",
    "--output",
    "--target-dir",
}


class IPv6ThreadingHTTPServer(ThreadingHTTPServer):
    address_family = socket.AF_INET6


class BridgeError(Exception):
    def __init__(self, status: HTTPStatus, message: str) -> None:
        super().__init__(message)
        self.status = status
        self.message = message


def secure_repo_root(value: str) -> Path:
    root = Path(value).expanduser().resolve(strict=True)
    if not root.is_dir():
        raise ValueError(f"repository root is not a directory: {root}")
    if not (root / ".git").exists():
        raise ValueError(f"repository root does not contain .git: {root}")
    return root


def validate_bind_host(value: str) -> str:
    host = value.strip()
    if not host:
        raise ValueError("bridge host must not be empty")
    if host.casefold() == "localhost":
        return host
    address_value = host[1:-1] if host.startswith("[") and host.endswith("]") else host
    try:
        address = ipaddress.ip_address(address_value)
    except ValueError as exc:
        raise ValueError("bridge host must be localhost or a loopback IP address") from exc
    if not address.is_loopback:
        raise ValueError("bridge host must be localhost or a loopback IP address")
    return address_value


def server_type_for_host(host: str) -> type[ThreadingHTTPServer]:
    try:
        address = ipaddress.ip_address(host)
    except ValueError:
        return ThreadingHTTPServer
    return IPv6ThreadingHTTPServer if address.version == 6 else ThreadingHTTPServer


def validate_extra_args(values: Any) -> list[str]:
    if values is None:
        return []
    if not isinstance(values, list) or not all(isinstance(item, str) for item in values):
        raise BridgeError(HTTPStatus.BAD_REQUEST, "args must be a list of strings")
    if len(values) > MAX_ARGUMENTS:
        raise BridgeError(HTTPStatus.BAD_REQUEST, "too many arguments")
    total = 0
    result: list[str] = []
    for value in values:
        if "\x00" in value or "\n" in value or "\r" in value:
            raise BridgeError(HTTPStatus.BAD_REQUEST, "arguments may not contain control lines")
        total += len(value)
        if total > MAX_ARGUMENT_BYTES:
            raise BridgeError(HTTPStatus.BAD_REQUEST, "arguments are too large")
        result.append(value)
    return result


def _option_name(value: str) -> str:
    """Return an option's name while retaining support for ``--name=value``."""
    return value.split("=", 1)[0] if value.startswith("--") else value


def _is_repo_relative_path(value: str) -> bool:
    """Accept only plain paths that cannot escape the configured repository cwd."""
    if not value or value.startswith(("/", "\\", "~")):
        return False
    if len(value) >= 2 and value[1] == ":":
        return False
    normalized = value.replace("\\", "/")
    if normalized.startswith(":(") or any(part == ".." for part in normalized.split("/")):
        return False
    return True


def _reject_unsafe_option(value: str) -> None:
    name = _option_name(value)
    if name in EXECUTABLE_OVERRIDE_OPTIONS or name in OUTPUT_OVERRIDE_OPTIONS:
        raise BridgeError(HTTPStatus.FORBIDDEN, f"argument is forbidden: {value}")
    # Git and Cargo accept short options with their value attached (for example
    # ``-xcommand`` or ``-cfoo.bar=baz``), so checking exact tokens is unsafe.
    if value in {"-x", "-o", "-c"} or value.startswith(("-x", "-o", "-c")):
        raise BridgeError(HTTPStatus.FORBIDDEN, f"argument is forbidden: {value}")


def _validate_git_diff_args(values: list[str]) -> None:
    path_mode = False
    allowed_options = {
        "--",
        "--cached",
        "--check",
        "--dirstat",
        "--histogram",
        "--minimal",
        "--name-only",
        "--name-status",
        "--no-color",
        "--no-ext-diff",
        "--no-renames",
        "--no-textconv",
        "--numstat",
        "--patch",
        "--patience",
        "--relative",
        "--shortstat",
        "--stat",
        "--staged",
        "--submodule",
        "--text",
        "--word-diff",
    }
    for value in values:
        _reject_unsafe_option(value)
        if path_mode:
            if value.startswith("-") or not _is_repo_relative_path(value):
                raise BridgeError(HTTPStatus.FORBIDDEN, f"path argument is forbidden: {value}")
            continue
        if value == "--":
            path_mode = True
        elif value.startswith("-"):
            if value not in allowed_options and not value.startswith(("--color=", "--relative=", "--word-diff=")):
                raise BridgeError(HTTPStatus.BAD_REQUEST, f"unsupported git.diff argument: {value}")
        elif not _is_repo_relative_path(value):
            raise BridgeError(HTTPStatus.FORBIDDEN, f"path argument is forbidden: {value}")


def _validate_cargo_args(values: list[str]) -> None:
    # These actions are deliberately limited to Cargo's package and display
    # selectors. In particular, a read-only check must not redirect build
    # output or select an alternate manifest outside the repository.
    value_for: set[str] = {"-p", "--package", "--exclude", "--features", "--target"}
    allowed = {
        "--all-features",
        "--all-targets",
        "--benches",
        "--bins",
        "--examples",
        "--frozen",
        "--ignore-rust-version",
        "--lib",
        "--locked",
        "--no-default-features",
        "--offline",
        "--release",
        "--tests",
        "--workspace",
        "--no-deps",
        "--document-private-items",
    }
    index = 0
    after_separator = False
    while index < len(values):
        value = values[index]
        _reject_unsafe_option(value)
        if value == "--":
            after_separator = True
            index += 1
            continue
        if after_separator:
            # Test/doc filters are values, but may not be used to smuggle a
            # path or another Cargo option into a subprocess invocation.
            if value.startswith("-") or not _is_repo_relative_path(value):
                raise BridgeError(HTTPStatus.FORBIDDEN, f"argument is forbidden: {value}")
            index += 1
            continue
        name = _option_name(value)
        if name in value_for:
            inline = "=" in value
            if inline:
                argument = value.split("=", 1)[1]
            else:
                index += 1
                if index >= len(values):
                    raise BridgeError(HTTPStatus.BAD_REQUEST, f"missing value for {value}")
                argument = values[index]
            if not argument or argument.startswith("-") or not _is_repo_relative_path(argument):
                raise BridgeError(HTTPStatus.FORBIDDEN, f"argument value is forbidden: {argument}")
        elif name == "--manifest-path":
            raise BridgeError(HTTPStatus.FORBIDDEN, "--manifest-path is not permitted")
        elif value.startswith("-") and value not in allowed:
            raise BridgeError(HTTPStatus.BAD_REQUEST, f"unsupported Cargo argument: {value}")
        elif not _is_repo_relative_path(value):
            raise BridgeError(HTTPStatus.FORBIDDEN, f"argument is forbidden: {value}")
        index += 1


def _validate_git_path_args(values: list[str]) -> None:
    for value in values:
        _reject_unsafe_option(value)
        if value == "--":
            continue
        if value.startswith("-"):
            if value not in {"-A", "--all", "-u", "--update", "-n", "--dry-run", "--intent-to-add", "--force"}:
                raise BridgeError(HTTPStatus.BAD_REQUEST, f"unsupported git.add argument: {value}")
        elif not _is_repo_relative_path(value):
            raise BridgeError(HTTPStatus.FORBIDDEN, f"path argument is forbidden: {value}")


def _validate_git_commit_args(values: list[str]) -> None:
    for value in values:
        _reject_unsafe_option(value)
        if value in {"-F", "--file", "-C", "--cleanup", "--author", "--date"} or value.startswith(("-F", "--file=", "-C")):
            raise BridgeError(HTTPStatus.FORBIDDEN, f"argument is forbidden: {value}")


def validate_action_args(action_name: str, action: Action, values: list[str]) -> list[str]:
    if not values:
        return values
    if not action.allow_extra:
        raise BridgeError(HTTPStatus.BAD_REQUEST, f"action does not accept extra arguments: {action_name}")
    if action.argument_policy == "cargo":
        _validate_cargo_args(values)
    elif action.argument_policy == "git.diff":
        _validate_git_diff_args(values)
    elif action.argument_policy == "git.paths":
        _validate_git_path_args(values)
    elif action.argument_policy == "git.commit":
        _validate_git_commit_args(values)
    elif action.argument_policy in {"git.generic", "gh"}:
        for value in values:
            _reject_unsafe_option(value)
            if not value.startswith("-") and (value.startswith(("/", "\\", "~")) or ".." in value.split("/")):
                raise BridgeError(HTTPStatus.FORBIDDEN, f"path argument is forbidden: {value}")
    else:
        raise BridgeError(HTTPStatus.BAD_REQUEST, f"action does not accept extra arguments: {action_name}")
    return values


def redact_env() -> dict[str, str]:
    allowed = {"PATH", "HOME", "USER", "TMPDIR", "TEMP", "TMP", "LANG", "LC_ALL", "TERM"}
    env = {key: value for key, value in os.environ.items() if key in allowed}
    env["CARGO_TERM_COLOR"] = "never"
    env["GIT_TERMINAL_PROMPT"] = "0"
    return env


class BoundedCapture:
    def __init__(self, limit: int) -> None:
        self.limit = limit
        self.data = bytearray()
        self.truncated = False
        self._lock = threading.Lock()

    def append(self, chunk: bytes) -> None:
        with self._lock:
            remaining = self.limit - len(self.data)
            if remaining > 0:
                self.data.extend(chunk[:remaining])
            if len(chunk) > max(remaining, 0):
                self.truncated = True


def _drain_output(stream: Any, capture: BoundedCapture) -> None:
    try:
        while True:
            chunk = stream.read(PROCESS_READ_CHUNK_BYTES)
            if not chunk:
                return
            capture.append(chunk)
    finally:
        stream.close()


def _terminate_process_tree(process: subprocess.Popen[bytes]) -> None:
    """Terminate a child and all descendants that inherited its output pipes."""
    if process.poll() is not None:
        return
    if os.name == "nt":
        # taskkill /T handles descendants that do not inherit CTRL_BREAK events.
        try:
            subprocess.run(
                ["taskkill", "/PID", str(process.pid), "/T", "/F"],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=PROCESS_TERMINATION_GRACE_SECONDS,
                check=False,
            )
        except (OSError, subprocess.TimeoutExpired):
            process.kill()
    else:
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            process.wait(timeout=PROCESS_TERMINATION_GRACE_SECONDS)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        else:
            # The direct child may have exited while a descendant retained the
            # process group and its pipes. Reap any such descendant after the
            # parent is gone; killpg is a no-op once the group is empty.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
    try:
        process.wait(timeout=PROCESS_TERMINATION_GRACE_SECONDS)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


class ProcessRegistry:
    """Tracks active bridge children so server shutdown can stop them."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._active: set[subprocess.Popen[bytes]] = set()

    def add(self, process: subprocess.Popen[bytes]) -> None:
        with self._lock:
            self._active.add(process)

    def remove(self, process: subprocess.Popen[bytes]) -> None:
        with self._lock:
            self._active.discard(process)

    def terminate_all(self) -> None:
        with self._lock:
            active = tuple(self._active)
        for process in active:
            _terminate_process_tree(process)


def append_audit(path: Path, record: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    line = json.dumps(record, sort_keys=True, separators=(",", ":")) + "\n"
    with path.open("a", encoding="utf-8") as handle:
        handle.write(line)
        handle.flush()
        os.fsync(handle.fileno())


def run_action(
    *,
    action_name: str,
    args: Any,
    repo_root: Path,
    allow_mutation: bool,
    timeout_seconds: int,
    max_output_bytes: int,
    process_registry: ProcessRegistry | None = None,
) -> dict[str, Any]:
    action = ACTIONS.get(action_name)
    if action is None:
        raise BridgeError(HTTPStatus.NOT_FOUND, f"unknown action: {action_name}")
    if action.mutating and not allow_mutation:
        raise BridgeError(HTTPStatus.FORBIDDEN, "mutating actions are disabled")

    extra = validate_action_args(action_name, action, validate_extra_args(args))

    argv = [*action.argv, *extra]
    started = time.monotonic()
    popen_kwargs: dict[str, Any] = {
        "cwd": repo_root,
        "env": redact_env(),
        "stdin": subprocess.DEVNULL,
        "stdout": subprocess.PIPE,
        "stderr": subprocess.PIPE,
    }
    if os.name == "nt":
        popen_kwargs["creationflags"] = getattr(subprocess, "CREATE_NEW_PROCESS_GROUP", 0)
    else:
        popen_kwargs["start_new_session"] = True
    try:
        process = subprocess.Popen(argv, **popen_kwargs)
        if process_registry is not None:
            process_registry.add(process)
        stdout_capture = BoundedCapture(max_output_bytes)
        stderr_capture = BoundedCapture(max_output_bytes)
        stdout_thread = threading.Thread(
            target=_drain_output,
            args=(process.stdout, stdout_capture),
            name="medusa-bridge-stdout",
            daemon=True,
        )
        stderr_thread = threading.Thread(
            target=_drain_output,
            args=(process.stderr, stderr_capture),
            name="medusa-bridge-stderr",
            daemon=True,
        )
        stdout_thread.start()
        stderr_thread.start()
        try:
            process.wait(timeout=timeout_seconds)
            timed_out = False
        except subprocess.TimeoutExpired:
            _terminate_process_tree(process)
            timed_out = True
        stdout_thread.join(timeout=PROCESS_TERMINATION_GRACE_SECONDS)
        stderr_thread.join(timeout=PROCESS_TERMINATION_GRACE_SECONDS)
        if stdout_thread.is_alive() or stderr_thread.is_alive():
            # A detached descendant may retain a pipe after its parent is gone;
            # close our handles so a request cannot retain reader threads.
            for stream in (process.stdout, process.stderr):
                if stream is not None:
                    stream.close()
            stdout_thread.join(timeout=PROCESS_TERMINATION_GRACE_SECONDS)
            stderr_thread.join(timeout=PROCESS_TERMINATION_GRACE_SECONDS)
    except FileNotFoundError as exc:
        raise BridgeError(HTTPStatus.FAILED_DEPENDENCY, f"executable not found: {argv[0]}") from exc
    finally:
        if "process" in locals() and process_registry is not None:
            process_registry.remove(process)

    stdout = bytes(stdout_capture.data).decode("utf-8", errors="replace")
    stderr = bytes(stderr_capture.data).decode("utf-8", errors="replace")
    return {
        "action": action_name,
        "argv": argv,
        "exit_code": 124 if timed_out else process.returncode,
        "success": not timed_out and process.returncode == 0,
        "timed_out": timed_out,
        "duration_ms": round((time.monotonic() - started) * 1000),
        "stdout": stdout,
        "stderr": stderr,
        "stdout_truncated": stdout_capture.truncated,
        "stderr_truncated": stderr_capture.truncated,
    }


class BridgeState:
    def __init__(
        self,
        *,
        token: str,
        repo_root: Path,
        allow_mutation: bool,
        timeout_seconds: int,
        max_output_bytes: int,
        audit_log: Path,
    ) -> None:
        self.token = token
        self.repo_root = repo_root
        self.allow_mutation = allow_mutation
        self.timeout_seconds = timeout_seconds
        self.max_output_bytes = max_output_bytes
        self.audit_log = audit_log
        self.lock = threading.Lock()
        self.process_registry = ProcessRegistry()


class Handler(BaseHTTPRequestHandler):
    server_version = "MedusaLocalBridge/1"

    @property
    def state(self) -> BridgeState:
        return self.server.state  # type: ignore[attr-defined]

    def log_message(self, fmt: str, *args: Any) -> None:
        sys.stderr.write("bridge: " + (fmt % args) + "\n")

    def send_json(self, status: HTTPStatus, payload: dict[str, Any]) -> None:
        encoded = json.dumps(payload, sort_keys=True).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(encoded)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(encoded)

    def authenticate(self) -> None:
        expected = f"Bearer {self.state.token}"
        supplied = self.headers.get("Authorization", "")
        if not hmac.compare_digest(supplied, expected):
            raise BridgeError(HTTPStatus.UNAUTHORIZED, "invalid bearer token")

    def read_json(self) -> dict[str, Any]:
        raw_length = self.headers.get("Content-Length")
        if raw_length is None:
            raise BridgeError(HTTPStatus.LENGTH_REQUIRED, "Content-Length is required")
        try:
            length = int(raw_length)
        except ValueError as exc:
            raise BridgeError(HTTPStatus.BAD_REQUEST, "invalid Content-Length") from exc
        if length < 0 or length > MAX_BODY_BYTES:
            raise BridgeError(HTTPStatus.REQUEST_ENTITY_TOO_LARGE, "request body is too large")
        try:
            value = json.loads(self.rfile.read(length))
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise BridgeError(HTTPStatus.BAD_REQUEST, "request body must be valid JSON") from exc
        if not isinstance(value, dict):
            raise BridgeError(HTTPStatus.BAD_REQUEST, "request body must be a JSON object")
        return value

    def do_GET(self) -> None:  # noqa: N802
        try:
            self.authenticate()
            if self.path != "/health":
                raise BridgeError(HTTPStatus.NOT_FOUND, "not found")
            self.send_json(
                HTTPStatus.OK,
                {
                    "ok": True,
                    "repo_root": str(self.state.repo_root),
                    "allow_mutation": self.state.allow_mutation,
                    "actions": sorted(ACTIONS),
                },
            )
        except BridgeError as exc:
            self.send_json(exc.status, {"ok": False, "error": exc.message})

    def do_POST(self) -> None:  # noqa: N802
        request_id = secrets.token_hex(12)
        started_at = time.time()
        action_name = ""
        try:
            self.authenticate()
            if self.path != "/v1/run":
                raise BridgeError(HTTPStatus.NOT_FOUND, "not found")
            body = self.read_json()
            action_name = body.get("action", "")
            if not isinstance(action_name, str) or not action_name:
                raise BridgeError(HTTPStatus.BAD_REQUEST, "action must be a non-empty string")
            with self.state.lock:
                result = run_action(
                    action_name=action_name,
                    args=body.get("args"),
                    repo_root=self.state.repo_root,
                    allow_mutation=self.state.allow_mutation,
                    timeout_seconds=self.state.timeout_seconds,
                    max_output_bytes=self.state.max_output_bytes,
                    process_registry=self.state.process_registry,
                )
            result["ok"] = True
            result["request_id"] = request_id
            append_audit(
                self.state.audit_log,
                {
                    "request_id": request_id,
                    "timestamp": started_at,
                    "action": action_name,
                    "argv": result["argv"],
                    "exit_code": result["exit_code"],
                    "duration_ms": result["duration_ms"],
                },
            )
            self.send_json(HTTPStatus.OK, result)
        except BridgeError as exc:
            append_audit(
                self.state.audit_log,
                {
                    "request_id": request_id,
                    "timestamp": started_at,
                    "action": action_name,
                    "error": exc.message,
                    "http_status": int(exc.status),
                },
            )
            self.send_json(exc.status, {"ok": False, "request_id": request_id, "error": exc.message})
        except Exception as exc:  # defensive boundary for the local service
            append_audit(
                self.state.audit_log,
                {
                    "request_id": request_id,
                    "timestamp": started_at,
                    "action": action_name,
                    "error": type(exc).__name__,
                    "http_status": 500,
                },
            )
            self.send_json(
                HTTPStatus.INTERNAL_SERVER_ERROR,
                {"ok": False, "request_id": request_id, "error": "internal bridge error"},
            )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True, help="repository root to confine all commands to")
    parser.add_argument("--host", default="127.0.0.1", help="loopback listen address")
    parser.add_argument("--port", type=int, default=8765)
    parser.add_argument("--token", default=os.environ.get("MEDUSA_BRIDGE_TOKEN"))
    parser.add_argument("--token-file", type=Path)
    parser.add_argument("--allow-mutation", action="store_true")
    parser.add_argument("--timeout", type=int, default=DEFAULT_TIMEOUT_SECONDS)
    parser.add_argument("--max-output-bytes", type=int, default=DEFAULT_MAX_OUTPUT_BYTES)
    parser.add_argument("--audit-log", type=Path)
    return parser.parse_args()


def resolve_token(args: argparse.Namespace) -> str:
    token = args.token
    if args.token_file:
        token = args.token_file.expanduser().read_text(encoding="utf-8").strip()
    if not token:
        raise ValueError("provide --token, --token-file, or MEDUSA_BRIDGE_TOKEN")
    if len(token) < 32:
        raise ValueError("bridge token must contain at least 32 characters")
    return token


def main() -> int:
    args = parse_args()
    try:
        repo_root = secure_repo_root(args.repo)
        token = resolve_token(args)
        host = validate_bind_host(args.host)
        if not 1 <= args.port <= 65535:
            raise ValueError("port must be between 1 and 65535")
        if args.timeout < 1 or args.timeout > 7200:
            raise ValueError("timeout must be between 1 and 7200 seconds")
        if args.max_output_bytes < 1024 or args.max_output_bytes > 16 * 1024 * 1024:
            raise ValueError("max output must be between 1 KiB and 16 MiB")
    except (OSError, ValueError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2

    audit_log = (args.audit_log or (repo_root / ".git" / "medusa-bridge-audit.jsonl")).expanduser()
    state = BridgeState(
        token=token,
        repo_root=repo_root,
        allow_mutation=args.allow_mutation,
        timeout_seconds=args.timeout,
        max_output_bytes=args.max_output_bytes,
        audit_log=audit_log,
    )
    server_type = server_type_for_host(host)
    server = server_type((host, args.port), Handler)
    server.state = state  # type: ignore[attr-defined]
    print(f"Medusa local bridge listening on http://{host}:{args.port}")
    print(f"Repository: {repo_root}")
    print(f"Mutating actions: {'enabled' if args.allow_mutation else 'disabled'}")
    print(f"Audit log: {audit_log}")

    def stop_on_signal(_signum: int, _frame: Any) -> None:
        # Raising in the serving thread enters the finally block below, where
        # every process group is terminated before the listening socket closes.
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, stop_on_signal)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        state.process_registry.terminate_all()
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
