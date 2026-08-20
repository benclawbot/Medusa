#!/usr/bin/env python3
"""Cross-platform, bounded live-provider product dogfood harness."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import shutil
import signal
import subprocess
import sys
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterable, Sequence


def configure_utf8_stdio() -> None:
    """Keep harness diagnostics printable on Windows hosts using CP1252."""
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is not None:
            reconfigure(encoding="utf-8", errors="replace")


def load_primary_dogfood_route() -> tuple[dict[str, object], dict[str, object]]:
    manifest_path = Path(__file__).resolve().parents[1] / "docs/provider-support.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    primary = [
        provider
        for provider in manifest.get("providers", [])
        if provider.get("dogfood", {}).get("status") == "primary"
    ]
    if len(primary) != 1:
        raise RuntimeError("provider support manifest must declare exactly one primary dogfood route")
    provider = primary[0]
    dogfood = provider.get("dogfood", {})
    required = ("model", "protocol", "base_url", "auth")
    if not provider.get("credential_environment") or any(not dogfood.get(key) for key in required):
        raise RuntimeError("primary dogfood route is incomplete")
    return provider, dogfood


PRIMARY_PROVIDER, PRIMARY_DOGFOOD = load_primary_dogfood_route()
PROVIDER = str(PRIMARY_PROVIDER["id"])
MODEL = str(PRIMARY_DOGFOOD["model"])
CREDENTIAL_ENVIRONMENT = str(PRIMARY_PROVIDER["credential_environment"])
DOGFOOD_PROTOCOL = str(PRIMARY_DOGFOOD["protocol"])
DOGFOOD_BASE_URL = str(PRIMARY_DOGFOOD["base_url"])
DOGFOOD_AUTH = str(PRIMARY_DOGFOOD["auth"])
ASSERTION_COUNT = 3
DEFAULT_TIMEOUT_SECONDS = 1500
DEFAULT_HEARTBEAT_SECONDS = 60
SCHEMA_VERSION = 1
MAX_TURNS = 16
PARALLEL_WORKERS = 2
MAX_OUTPUT_TOKENS = 4096
CONTEXT_WINDOW_TOKENS = 32768
MAX_RETRIES = 2
INPUT_COST_MICROUSD_PER_MILLION = 5_000_000
OUTPUT_COST_MICROUSD_PER_MILLION = 20_000_000
MAX_COST_MICROUSD = 20_000_000
ALLOWED_FAILURE_CLASSES = {"product", "provider", "environment", "flaky-test"}


class HarnessError(RuntimeError):
    def __init__(self, message: str, classification: str = "product") -> None:
        super().__init__(message)
        self.classification = classification


def platform_name() -> str:
    return {"Linux": "Linux", "Darwin": "macOS", "Windows": "Windows"}.get(
        platform.system(), platform.system() or "unknown"
    )


def sanitize(text: str, secrets: Iterable[str]) -> str:
    sanitized = text
    for secret in secrets:
        if secret:
            sanitized = sanitized.replace(secret, "[REDACTED]")
    return sanitized


def classify_failure(message: str) -> str:
    lowered = message.lower()
    if any(token in lowered for token in (
        "api key", "credential", "not found", "no such file", "required executable",
    )):
        return "environment"
    if any(token in lowered for token in (
        "runner lost", "the operation was canceled", "temporary runner", "artifact service",
    )):
        return "flaky-test"
    if any(token in lowered for token in (
        "401", "403", "429", "rate limit", "provider", PROVIDER.lower(), "timed out",
        "connection reset", "service unavailable",
    )):
        return "provider"
    return "product"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def run_checked(
    command: Sequence[str],
    *,
    cwd: Path,
    env: dict[str, str] | None = None,
    capture: bool = False,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        list(command),
        cwd=cwd,
        env=env,
        check=True,
        text=True,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.STDOUT if capture else None,
    )


def terminate_process_tree(process: subprocess.Popen[str]) -> None:
    if process.poll() is not None:
        return
    if os.name == "nt":
        subprocess.run(
            ["taskkill", "/PID", str(process.pid), "/T", "/F"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
    else:
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            return
        try:
            process.wait(timeout=10)
            return
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=10)


@dataclass
class Harness:
    repo_root: Path
    output_dir: Path
    timeout_seconds: int
    heartbeat_seconds: int
    api_key: str
    started: float = field(default_factory=time.monotonic)
    phase: str = "initialization"
    work_root: Path = field(init=False)
    fixture: Path = field(init=False)
    isolated_home: Path = field(init=False)
    log_lines: list[str] = field(default_factory=list)
    credential_audited: bool = False
    usage: dict[str, int] = field(default_factory=dict)
    baseline_commit: str | None = None

    def __post_init__(self) -> None:
        self.work_root = self.repo_root / "target" / "live-e2e-work" / platform_name().lower()
        self.fixture = self.work_root / "multi-language-repair"
        self.isolated_home = self.work_root / "home"

    @property
    def elapsed_seconds(self) -> int:
        return int(time.monotonic() - self.started)

    @property
    def secrets(self) -> tuple[str, ...]:
        return (self.api_key,)

    def log(self, message: str) -> None:
        safe = sanitize(message, self.secrets)
        print(safe, flush=True)
        self.log_lines.append(safe)

    def set_phase(self, phase: str) -> None:
        self.phase = phase
        self.log(f"[live-e2e] phase={phase} elapsed={self.elapsed_seconds}s")

    def child_environment(self) -> dict[str, str]:
        env = os.environ.copy()
        env.update(
            {
                "HOME": str(self.isolated_home),
                "USERPROFILE": str(self.isolated_home),
                "XDG_CONFIG_HOME": str(self.isolated_home / ".config"),
                "XDG_CACHE_HOME": str(self.isolated_home / ".cache"),
                "APPDATA": str(self.isolated_home / "AppData" / "Roaming"),
                "LOCALAPPDATA": str(self.isolated_home / "AppData" / "Local"),
                "PYTHONUTF8": "1",
                CREDENTIAL_ENVIRONMENT: self.api_key,
                "MEDUSA_INPUT_COST_MICROUSD_PER_MILLION": str(INPUT_COST_MICROUSD_PER_MILLION),
                "MEDUSA_OUTPUT_COST_MICROUSD_PER_MILLION": str(OUTPUT_COST_MICROUSD_PER_MILLION),
                "MEDUSA_CACHE_READ_COST_MICROUSD_PER_MILLION": str(INPUT_COST_MICROUSD_PER_MILLION),
                "MEDUSA_CACHE_WRITE_COST_MICROUSD_PER_MILLION": str(INPUT_COST_MICROUSD_PER_MILLION),
            }
        )
        return env

    def validate_budgets(self) -> None:
        per_turn = (
            CONTEXT_WINDOW_TOKENS * INPUT_COST_MICROUSD_PER_MILLION * 3
            + MAX_OUTPUT_TOKENS * OUTPUT_COST_MICROUSD_PER_MILLION
            + 999_999
        ) // 1_000_000
        theoretical = per_turn * MAX_TURNS * PARALLEL_WORKERS
        if theoretical > MAX_COST_MICROUSD:
            raise HarnessError(
                f"configured theoretical cost exceeds budget: {theoretical} > {MAX_COST_MICROUSD}",
                "environment",
            )

    def prepare(self) -> None:
        self.set_phase("prepare-output")
        shutil.rmtree(self.work_root, ignore_errors=True)
        shutil.rmtree(self.output_dir, ignore_errors=True)
        self.output_dir.mkdir(parents=True, exist_ok=True)
        self.fixture.mkdir(parents=True, exist_ok=True)
        self.isolated_home.mkdir(parents=True, exist_ok=True)
        (self.work_root / "launch").mkdir(parents=True, exist_ok=True)
        self.validate_budgets()

        self.set_phase("build-medusa")
        run_checked(
            ["cargo", "build", "--release", "--locked", "--bin", "medusa"],
            cwd=self.repo_root,
        )

        self.set_phase("stage-installed-binary")
        installed = self.work_root / "installed" / "bin"
        installed.mkdir(parents=True, exist_ok=True)
        source_binary = self.repo_root / "target" / "release" / ("medusa.exe" if os.name == "nt" else "medusa")
        if not source_binary.is_file():
            raise HarnessError(f"built Medusa binary not found at {source_binary}", "environment")
        shutil.copy2(source_binary, installed / source_binary.name)
        if os.name != "nt":
            (installed / source_binary.name).chmod(0o755)

        self.set_phase("prepare-fixture")
        (self.fixture / "src").mkdir(parents=True)
        (self.fixture / ".medusa" / "sessions").mkdir(parents=True)
        (self.fixture / "value.txt").write_text("41\n", encoding="utf-8")
        (self.fixture / "src" / "slugify.py").write_text(
            'def slugify(value: str) -> str:\n    raise NotImplementedError("implement me")\n',
            encoding="utf-8",
        )
        (self.fixture / "src" / "counter.js").write_text(
            "export function applyCounter(state, action) {\n"
            "  if (action.type === 'increment') return { count: state.count - 1 };\n"
            "  if (action.type === 'decrement') return { count: state.count + 1 };\n"
            "  return state;\n"
            "}\n",
            encoding="utf-8",
        )
        (self.fixture / "package.json").write_text(
            '{"type":"module","scripts":{"test":"node test.mjs"}}\n', encoding="utf-8"
        )
        (self.fixture / "test.mjs").write_text(
            "import assert from 'node:assert/strict';\n"
            "import { applyCounter } from './src/counter.js';\n"
            "assert.deepEqual(applyCounter({count: 2}, {type: 'increment'}), {count: 3});\n"
            "assert.deepEqual(applyCounter({count: 2}, {type: 'decrement'}), {count: 1});\n"
            "const original = {count: 2};\n"
            "assert.equal(applyCounter(original, {type: 'noop'}), original);\n"
            "console.log('verified-javascript-counter');\n",
            encoding="utf-8",
        )
        (self.fixture / "verify.py").write_text(
            "from pathlib import Path\n"
            "import shutil\n"
            "import subprocess\n"
            "\n"
            "assert Path('value.txt').read_text(encoding='utf-8').strip() == '42'\n"
            "print('verified-rust-value-fix')\n"
            "from src.slugify import slugify\n"
            "assert slugify('Hello, World!') == 'hello-world'\n"
            "assert slugify('  Multiple   spaces  ') == 'multiple-spaces'\n"
            "assert slugify('Already-Slugged') == 'already-slugged'\n"
            "assert slugify('Crème brûlée') == 'creme-brulee'\n"
            "print('verified-python-slugify')\n"
            "npm = shutil.which('npm') or shutil.which('npm.cmd')\n"
            "assert npm, 'npm is required'\n"
            "subprocess.run([npm, '--no-update-notifier', 'test'], check=True)\n",
            encoding="utf-8",
        )
        run_checked(["git", "init", "-q", "-b", "main"], cwd=self.fixture)
        run_checked(["git", "config", "user.name", "Medusa Live E2E"], cwd=self.fixture)
        run_checked(
            ["git", "config", "user.email", "medusa-e2e@example.invalid"], cwd=self.fixture
        )
        run_checked(["git", "add", "-A"], cwd=self.fixture)
        run_checked(["git", "commit", "-q", "-m", "baseline"], cwd=self.fixture)
        baseline = run_checked(["git", "rev-parse", "HEAD"], cwd=self.fixture, capture=True)
        self.baseline_commit = (baseline.stdout or "").strip()
        if not self.baseline_commit:
            raise HarnessError("fixture baseline commit identity is missing", "environment")

    def protected_hashes(self) -> dict[str, str]:
        return {
            name: sha256(self.fixture / name)
            for name in ("verify.py", "test.mjs", "package.json")
        }

    def binary_path(self) -> Path:
        suffix = ".exe" if os.name == "nt" else ""
        return self.work_root / "installed" / "bin" / f"medusa{suffix}"

    def medusa_binary(self) -> Path:
        binary = self.binary_path()
        if not binary.is_file():
            raise HarnessError(f"built Medusa binary not found at {binary}", "environment")
        return binary

    def run_agent(self) -> None:
        self.set_phase("autonomous-session")
        objective = (
            "Inspect this repository and repair all three product defects without modifying "
            "verify.py, test.mjs, package.json, fixtures, or expected outputs. Correct value.txt "
            "to the verified value, robustly implement src/slugify.py while preserving its public "
            "API, and repair the counter transitions in src/counter.js. Run `python verify.py`, "
            "iterate until every check passes, and stop only after all three independent "
            "validations succeed."
        )
        command = [
            str(self.medusa_binary()),
            "--repo",
            str(self.fixture),
            "--set",
            f"agent.max_turns={MAX_TURNS}",
            "--set",
            f"agent.parallel_workers={PARALLEL_WORKERS}",
            "--set",
            f"model.provider={PROVIDER}",
            "--set",
            f"model.name={MODEL}",
            "--set",
            f"model.protocol={DOGFOOD_PROTOCOL}",
            "--set",
            f"model.base_url={DOGFOOD_BASE_URL}",
            "--set",
            f"model.auth={DOGFOOD_AUTH}",
            "--set",
            "model.tool_calling=true",
            "--set",
            "model.streaming=false",
            "--set",
            f"model.max_output_tokens={MAX_OUTPUT_TOKENS}",
            "--set",
            f"model.context_window_tokens={CONTEXT_WINDOW_TOKENS}",
            "--set",
            f"model.max_retries={MAX_RETRIES}",
            "--set",
            "model.retry_base_delay_ms=500",
            "--set",
            "model.retry_max_delay_ms=8000",
            "--set",
            "model.retry_jitter_ms=100",
            "--set",
            "verification.required=true",
            "run",
            objective,
        ]
        creationflags = subprocess.CREATE_NEW_PROCESS_GROUP if os.name == "nt" else 0
        process = subprocess.Popen(
            command,
            cwd=self.work_root / "launch",
            env=self.child_environment(),
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            encoding="utf-8",
            errors="replace",
            bufsize=1,
            start_new_session=os.name != "nt",
            creationflags=creationflags,
        )
        stop = threading.Event()

        def heartbeat() -> None:
            while not stop.wait(self.heartbeat_seconds):
                self.log(
                    f"[live-e2e] heartbeat phase={self.phase} "
                    f"elapsed={self.elapsed_seconds}s session=multi-language-repair"
                )

        def capture_output() -> None:
            assert process.stdout is not None
            for line in process.stdout:
                self.log(line.rstrip("\n"))

        heartbeat_thread = threading.Thread(target=heartbeat, daemon=True)
        output_thread = threading.Thread(target=capture_output, daemon=True)
        heartbeat_thread.start()
        output_thread.start()
        try:
            status = process.wait(timeout=self.timeout_seconds)
        except subprocess.TimeoutExpired as error:
            terminate_process_tree(process)
            raise HarnessError(
                f"autonomous session timed out after {self.timeout_seconds}s",
                "provider",
            ) from error
        finally:
            stop.set()
            heartbeat_thread.join(timeout=5)
            output_thread.join(timeout=10)
        if status != 0:
            tail = "\n".join(self.log_lines[-50:])
            raise HarnessError(
                f"autonomous session exited with status {status}: {tail}",
                classify_failure(tail),
            )

    def verify(self, before: dict[str, str]) -> None:
        self.set_phase("verify-contract-integrity")
        after = self.protected_hashes()
        if before != after:
            raise HarnessError("immutable verification contract changed", "product")

        self.set_phase("run-independent-verification")
        result = run_checked(
            [sys.executable, "verify.py"], cwd=self.fixture, capture=True
        )
        assert result.stdout is not None
        for line in result.stdout.splitlines():
            self.log(line)
        required = {
            "verified-rust-value-fix",
            "verified-python-slugify",
            "verified-javascript-counter",
        }
        missing = sorted(marker for marker in required if marker not in result.stdout)
        if missing:
            raise HarnessError(f"independent verification missed markers: {missing}")

    def collect_usage(self) -> None:
        self.set_phase("usage-and-cost-audit")
        total_tokens = 0
        estimated_cost_microusd = 0
        model_turns = 0
        session_paths = sorted((self.fixture / ".medusa" / "sessions").glob("*.json"))
        for path in session_paths:
            try:
                session = json.loads(path.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError) as error:
                raise HarnessError(f"could not parse durable session usage {path}: {error}") from error
            for event in session.get("events", []):
                payload = event.get("payload", {})
                if payload.get("type") != "model_response_received":
                    continue
                usage = payload.get("data", {}).get("usage", {})
                try:
                    total_tokens += int(usage.get("total_tokens", 0))
                    estimated_cost_microusd += int(usage.get("estimated_cost_microusd", 0))
                    model_turns += 1
                except (TypeError, ValueError) as error:
                    raise HarnessError(f"invalid durable usage payload in {path}: {usage}") from error
        if model_turns == 0 or total_tokens <= 0:
            raise HarnessError("durable model usage evidence is missing", "product")
        if model_turns > MAX_TURNS * PARALLEL_WORKERS:
            raise HarnessError(
                f"model turn budget exceeded: {model_turns} > {MAX_TURNS * PARALLEL_WORKERS}",
                "product",
            )
        if estimated_cost_microusd > MAX_COST_MICROUSD:
            raise HarnessError(
                f"estimated cost budget exceeded: {estimated_cost_microusd} > {MAX_COST_MICROUSD} microusd",
                "product",
            )
        self.usage = {
            "model_turns": model_turns,
            "total_tokens": total_tokens,
            "estimated_cost_microusd": estimated_cost_microusd,
        }

    def product_patch(self) -> str:
        head_result = run_checked(["git", "rev-parse", "HEAD"], cwd=self.fixture, capture=True)
        head = (head_result.stdout or "").strip()
        if self.baseline_commit and head and head != self.baseline_commit:
            patch = run_checked(
                ["git", "diff", "--binary", self.baseline_commit, head, "--"],
                cwd=self.fixture,
                capture=True,
            )
            return patch.stdout or ""
        patch = run_checked(
            ["git", "diff", "--binary", "HEAD", "--"], cwd=self.fixture, capture=True
        )
        return patch.stdout or ""

    def collect(self) -> None:
        self.set_phase("collect-evidence")
        session_dir = self.output_dir / "multi-language-repair"
        session_dir.mkdir(parents=True, exist_ok=True)
        patch = self.product_patch()
        status = run_checked(["git", "status", "--short"], cwd=self.fixture, capture=True)
        (session_dir / "change.patch").write_text(patch, encoding="utf-8")
        (session_dir / "status.txt").write_text(status.stdout or "", encoding="utf-8")
        sessions = self.fixture / ".medusa" / "sessions"
        if sessions.is_dir():
            shutil.copytree(sessions, session_dir / "sessions", dirs_exist_ok=True)
        (self.output_dir / "multi-language-repair.log").write_text(
            "\n".join(self.log_lines) + "\n", encoding="utf-8"
        )

    def assert_secret_not_persisted(self) -> None:
        self.set_phase("credential-persistence-audit")
        needle = self.api_key.encode("utf-8")
        roots = (self.isolated_home, self.fixture, self.output_dir)
        for root in roots:
            if not root.exists():
                continue
            for path in root.rglob("*"):
                if not path.is_file():
                    continue
                try:
                    if needle in path.read_bytes():
                        raise HarnessError(
                            f"credential material was persisted in {path.relative_to(root)}",
                            "product",
                        )
                except OSError as error:
                    raise HarnessError(
                        f"could not audit persisted file {path}: {error}", "environment"
                    ) from error
        self.credential_audited = True

    def commit_sha(self) -> str:
        result = run_checked(
            ["git", "rev-parse", "HEAD"], cwd=self.repo_root, capture=True
        )
        return (result.stdout or "unknown").strip()

    def write_summary(self, *, result: str, classification: str | None, detail: str | None) -> None:
        binary = self.binary_path()
        summary = {
            "schema_version": SCHEMA_VERSION,
            "result": result,
            "classification": classification,
            "detail": sanitize(detail or "", self.secrets) or None,
            "commit": self.commit_sha(),
            "platform": platform_name(),
            "provider": PROVIDER,
            "model": MODEL,
            "sessions": 1,
            "passed": ASSERTION_COUNT if result == "passed" else 0,
            "build": {
                "binary_sha256": sha256(binary) if binary.is_file() else None,
                "architecture": platform.machine() or "unknown",
                "os_release": platform.release() or "unknown",
            },
            "usage": self.usage or None,
            "total": ASSERTION_COUNT,
            "credential_persisted": False if self.credential_audited else None,
            "verification_contract_unchanged": result == "passed",
            "bounded": {
                "timeout_seconds": self.timeout_seconds,
                "max_turns": MAX_TURNS,
                "parallel_workers": PARALLEL_WORKERS,
                "max_output_tokens": MAX_OUTPUT_TOKENS,
                "context_window_tokens": CONTEXT_WINDOW_TOKENS,
                "max_retries": MAX_RETRIES,
                "max_cost_microusd": MAX_COST_MICROUSD,
            },
            "phase": self.phase,
            "elapsed_seconds": self.elapsed_seconds,
        }
        self.output_dir.mkdir(parents=True, exist_ok=True)
        temporary = self.output_dir / "summary.json.tmp"
        temporary.write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        temporary.replace(self.output_dir / "summary.json")

    def run(self) -> None:
        before: dict[str, str] = {}
        try:
            self.prepare()
            before = self.protected_hashes()
            self.run_agent()
            self.verify(before)
            self.collect_usage()
            self.collect()
            self.assert_secret_not_persisted()
            self.set_phase("complete")
            self.write_summary(result="passed", classification=None, detail=None)
            self.log("live-coding-e2e-ok:3/3-in-one-session")
        except Exception as error:
            safe = sanitize(str(error), self.secrets)
            classification = (
                error.classification if isinstance(error, HarnessError) else classify_failure(safe)
            )
            try:
                self.collect()
            except Exception as collect_error:
                safe += f"; evidence collection failed: {sanitize(str(collect_error), self.secrets)}"
            try:
                self.assert_secret_not_persisted()
            except Exception as audit_error:
                shutil.rmtree(self.output_dir, ignore_errors=True)
                self.output_dir.mkdir(parents=True, exist_ok=True)
                safe += "; retained evidence was removed because the credential audit did not pass: "
                safe += sanitize(str(audit_error), self.secrets)
            self.write_summary(result="failed", classification=classification, detail=safe)
            raise
        finally:
            shutil.rmtree(self.work_root, ignore_errors=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=Path("live-e2e-artifacts"))
    parser.add_argument(
        "--timeout-seconds",
        type=int,
        default=int(os.environ.get("LIVE_E2E_TIMEOUT_SECONDS", DEFAULT_TIMEOUT_SECONDS)),
    )
    parser.add_argument(
        "--heartbeat-seconds",
        type=int,
        default=int(os.environ.get("LIVE_E2E_HEARTBEAT_SECONDS", DEFAULT_HEARTBEAT_SECONDS)),
    )
    return parser.parse_args()


def main() -> int:
    configure_utf8_stdio()
    args = parse_args()
    if args.timeout_seconds <= 0 or args.heartbeat_seconds <= 0:
        print("timeouts must be positive", file=sys.stderr)
        return 2
    api_key = os.environ.get(CREDENTIAL_ENVIRONMENT, "")
    if not api_key:
        print(
            f"{CREDENTIAL_ENVIRONMENT} is required for live coding end-to-end tests",
            file=sys.stderr,
        )
        return 2
    repo_root = Path(
        run_checked(["git", "rev-parse", "--show-toplevel"], cwd=Path.cwd(), capture=True).stdout.strip()
    )
    harness = Harness(
        repo_root=repo_root,
        output_dir=(repo_root / args.output).resolve() if not args.output.is_absolute() else args.output,
        timeout_seconds=args.timeout_seconds,
        heartbeat_seconds=args.heartbeat_seconds,
        api_key=api_key,
    )
    try:
        harness.run()
    except Exception as error:
        print(sanitize(f"live coding dogfood failed: {error}", harness.secrets), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
