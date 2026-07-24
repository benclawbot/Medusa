#!/usr/bin/env python3
"""Medusa local execution bridge.

A small, dependency-free localhost service that exposes a constrained set of
repository maintenance commands to an orchestrator. It never invokes a shell,
binds to loopback by default, requires a bearer token, confines execution to a
configured repository, and writes an append-only JSONL audit log.
"""

from __future__ import annotations

import argparse
import hmac
import json
import os
import secrets
import shlex
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


ACTIONS: Final[dict[str, Action]] = {
    "cargo.fmt": Action(("cargo", "fmt", "--all"), mutating=True),
    "cargo.fmt-check": Action(("cargo", "fmt", "--all", "--", "--check")),
    "cargo.generate-lockfile": Action(("cargo", "generate-lockfile"), mutating=True),
    "cargo.check": Action(("cargo", "check", "--workspace", "--all-targets"), allow_extra=True),
    "cargo.clippy": Action(
        ("cargo", "clippy", "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings"),
        allow_extra=False,
    ),
    "cargo.test": Action(("cargo", "test", "--workspace", "--all-features"), allow_extra=True),
    "cargo.doc": Action(("cargo", "doc", "--workspace", "--no-deps"), allow_extra=True),
    "git.status": Action(("git", "status", "--short", "--branch")),
    "git.diff": Action(("git", "diff", "--stat"), allow_extra=True),
    "git.diff-check": Action(("git", "diff", "--check")),
    "git.add": Action(("git", "add", "--"), mutating=True, allow_extra=True),
    "git.commit": Action(("git", "commit"), mutating=True, allow_extra=True),
    "git.push": Action(("git", "push"), mutating=True, allow_extra=True),
    "git.fetch": Action(("git", "fetch", "--prune"), mutating=True, allow_extra=True),
    "git.checkout": Action(("git", "checkout"), mutating=True, allow_extra=True),
    "git.rebase": Action(("git", "rebase"), mutating=True, allow_extra=True),
    "gh.auth-status": Action(("gh", "auth", "status")),
    "gh.pr-checks": Action(("gh", "pr", "checks"), allow_extra=True),
    "gh.run-view": Action(("gh", "run", "view"), allow_extra=True),
}

FORBIDDEN_EXTRA_TOKENS: Final = {
    "--exec",
    "-x",
    "--upload-pack",
    "--receive-pack",
    "--config-env",
    "--config",
    "-c",
}


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


def validate_extra_args(values: Any) -> list[str]:
    if values is None:
        return []
    if not isinstance(values, list) or not all(isinstance(item, str) for item in values):
        raise BridgeError(HTTPStatus.BAD_REQUEST, "args must be a list of strings")
    if len(values) > 64:
        raise BridgeError(HTTPStatus.BAD_REQUEST, "too many arguments")
    total = 0
    result: list[str] = []
    for value in values:
        if "\x00" in value or "\n" in value or "\r" in value:
            raise BridgeError(HTTPStatus.BAD_REQUEST, "arguments may not contain control lines")
        if value in FORBIDDEN_EXTRA_TOKENS or value.startswith("--config="):
            raise BridgeError(HTTPStatus.FORBIDDEN, f"argument is forbidden: {value}")
        total += len(value)
        if total > 8192:
            raise BridgeError(HTTPStatus.BAD_REQUEST, "arguments are too large")
        result.append(value)
    return result


def redact_env() -> dict[str, str]:
    allowed = {"PATH", "HOME", "USER", "TMPDIR", "TEMP", "TMP", "LANG", "LC_ALL", "TERM"}
    env = {key: value for key, value in os.environ.items() if key in allowed}
    env["CARGO_TERM_COLOR"] = "never"
    env["GIT_TERMINAL_PROMPT"] = "0"
    return env


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
) -> dict[str, Any]:
    action = ACTIONS.get(action_name)
    if action is None:
        raise BridgeError(HTTPStatus.NOT_FOUND, f"unknown action: {action_name}")
    if action.mutating and not allow_mutation:
        raise BridgeError(HTTPStatus.FORBIDDEN, "mutating actions are disabled")

    extra = validate_extra_args(args)
    if extra and not action.allow_extra:
        raise BridgeError(HTTPStatus.BAD_REQUEST, f"action does not accept extra arguments: {action_name}")

    argv = [*action.argv, *extra]
    started = time.monotonic()
    try:
        completed = subprocess.run(
            argv,
            cwd=repo_root,
            env=redact_env(),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout_seconds,
            check=False,
        )
        timed_out = False
    except FileNotFoundError as exc:
        raise BridgeError(HTTPStatus.FAILED_DEPENDENCY, f"executable not found: {argv[0]}") from exc
    except subprocess.TimeoutExpired as exc:
        stdout = exc.stdout or b""
        stderr = exc.stderr or b""
        completed = subprocess.CompletedProcess(argv, 124, stdout, stderr)
        timed_out = True

    def decode_limited(raw: bytes) -> tuple[str, bool]:
        truncated = len(raw) > max_output_bytes
        return raw[:max_output_bytes].decode("utf-8", errors="replace"), truncated

    stdout, stdout_truncated = decode_limited(completed.stdout)
    stderr, stderr_truncated = decode_limited(completed.stderr)
    return {
        "action": action_name,
        "argv": argv,
        "exit_code": completed.returncode,
        "success": completed.returncode == 0,
        "timed_out": timed_out,
        "duration_ms": round((time.monotonic() - started) * 1000),
        "stdout": stdout,
        "stderr": stderr,
        "stdout_truncated": stdout_truncated,
        "stderr_truncated": stderr_truncated,
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
    parser.add_argument("--host", default="127.0.0.1", help="listen address; loopback is strongly recommended")
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
    server = ThreadingHTTPServer((args.host, args.port), Handler)
    server.state = state  # type: ignore[attr-defined]
    print(f"Medusa local bridge listening on http://{args.host}:{args.port}")
    print(f"Repository: {repo_root}")
    print(f"Mutating actions: {'enabled' if args.allow_mutation else 'disabled'}")
    print(f"Audit log: {audit_log}")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
