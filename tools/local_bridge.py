#!/usr/bin/env python3
"""Authenticated localhost bridge for constrained Medusa maintenance commands."""

from __future__ import annotations

import argparse
import hmac
import json
import os
import secrets
import subprocess
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

MAX_BODY = 64 * 1024
MAX_OUTPUT = 2 * 1024 * 1024
DEFAULT_TIMEOUT = 900

ALLOWED: dict[str, tuple[str, ...]] = {
    "cargo_fmt": ("cargo", "fmt", "--all"),
    "cargo_fmt_check": ("cargo", "fmt", "--all", "--", "--check"),
    "cargo_check": ("cargo", "check", "--workspace", "--all-targets", "--locked"),
    "cargo_clippy": (
        "cargo", "clippy", "--workspace", "--all-targets", "--all-features", "--locked", "--", "-D", "warnings",
    ),
    "cargo_test": ("cargo", "test", "--workspace", "--all-targets", "--locked"),
    "cargo_generate_lockfile": ("cargo", "generate-lockfile"),
    "git_status": ("git", "status", "--short", "--branch"),
    "git_diff": ("git", "diff", "--"),
    "git_diff_cached": ("git", "diff", "--cached", "--"),
    "git_add_all": ("git", "add", "--all"),
    "git_push": ("git", "push"),
    "gh_pr_checks": ("gh", "pr", "checks"),
}


def safe_repo(value: str) -> Path:
    repo = Path(value).expanduser().resolve(strict=True)
    if not (repo / ".git").exists():
        raise ValueError(f"not a Git repository: {repo}")
    return repo


def sanitize_args(command: str, raw: Any) -> list[str]:
    if raw is None:
        return []
    if not isinstance(raw, list) or not all(isinstance(item, str) for item in raw):
        raise ValueError("args must be a list of strings")
    if len(raw) > 32 or any(len(item) > 512 or "\x00" in item for item in raw):
        raise ValueError("args exceed bridge limits")

    if command == "git_diff" or command == "git_diff_cached":
        if any(item.startswith("-") or Path(item).is_absolute() or ".." in Path(item).parts for item in raw):
            raise ValueError("git diff accepts repository-relative paths only")
        return raw
    if command == "gh_pr_checks":
        if len(raw) > 1 or (raw and not raw[0].isdigit()):
            raise ValueError("gh_pr_checks accepts one numeric PR number")
        return raw
    if raw:
        raise ValueError(f"{command} does not accept extra arguments")
    return []


def run_command(repo: Path, command: str, extra: list[str], timeout: int) -> dict[str, Any]:
    argv = [*ALLOWED[command], *extra]
    completed = subprocess.run(
        argv,
        cwd=repo,
        env={**os.environ, "GIT_TERMINAL_PROMPT": "0", "CARGO_TERM_COLOR": "never"},
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=timeout,
        check=False,
    )
    stdout = completed.stdout[-MAX_OUTPUT:]
    stderr = completed.stderr[-MAX_OUTPUT:]
    return {
        "ok": completed.returncode == 0,
        "command": command,
        "argv": argv,
        "returncode": completed.returncode,
        "stdout": stdout,
        "stderr": stderr,
        "truncated": len(completed.stdout) > MAX_OUTPUT or len(completed.stderr) > MAX_OUTPUT,
    }


def make_handler(repo: Path, token: str, timeout: int):
    class Handler(BaseHTTPRequestHandler):
        server_version = "MedusaLocalBridge/1"

        def log_message(self, fmt: str, *args: object) -> None:
            print(f"bridge: {self.address_string()} {fmt % args}", file=sys.stderr)

        def send_json(self, status: int, payload: dict[str, Any]) -> None:
            body = json.dumps(payload, separators=(",", ":")).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)

        def authorized(self) -> bool:
            supplied = self.headers.get("Authorization", "")
            expected = f"Bearer {token}"
            return hmac.compare_digest(supplied, expected)

        def do_GET(self) -> None:
            if self.path != "/health":
                self.send_json(404, {"ok": False, "error": "not found"})
                return
            self.send_json(200, {"ok": True, "repo": str(repo), "commands": sorted(ALLOWED)})

        def do_POST(self) -> None:
            if self.path != "/v1/run":
                self.send_json(404, {"ok": False, "error": "not found"})
                return
            if not self.authorized():
                self.send_json(401, {"ok": False, "error": "unauthorized"})
                return
            try:
                length = int(self.headers.get("Content-Length", "0"))
                if length <= 0 or length > MAX_BODY:
                    raise ValueError("invalid request size")
                payload = json.loads(self.rfile.read(length))
                command = payload.get("command")
                if command not in ALLOWED:
                    raise ValueError("command is not allowlisted")
                extra = sanitize_args(command, payload.get("args"))
                result = run_command(repo, command, extra, timeout)
                self.send_json(200, result)
            except subprocess.TimeoutExpired:
                self.send_json(408, {"ok": False, "error": "command timed out"})
            except (ValueError, json.JSONDecodeError) as exc:
                self.send_json(400, {"ok": False, "error": str(exc)})
            except Exception as exc:
                self.send_json(500, {"ok": False, "error": f"bridge failure: {exc}"})

    return Handler


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True, help="Repository root exposed to the bridge")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8765)
    parser.add_argument("--timeout", type=int, default=DEFAULT_TIMEOUT)
    args = parser.parse_args()

    repo = safe_repo(args.repo)
    token = os.environ.get("MEDUSA_BRIDGE_TOKEN") or secrets.token_urlsafe(32)
    if args.host not in {"127.0.0.1", "::1", "localhost"}:
        raise SystemExit("refusing non-loopback bind")
    if not 1 <= args.port <= 65535 or not 1 <= args.timeout <= 3600:
        raise SystemExit("invalid port or timeout")

    print(f"MEDUSA_BRIDGE_URL=http://{args.host}:{args.port}")
    print(f"MEDUSA_BRIDGE_TOKEN={token}")
    print(f"MEDUSA_BRIDGE_REPO={repo}")
    server = ThreadingHTTPServer((args.host, args.port), make_handler(repo, token, args.timeout))
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
