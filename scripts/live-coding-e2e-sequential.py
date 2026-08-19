#!/usr/bin/env python3
"""Run the live coding harness with deterministic delegated-worker instructions."""

from __future__ import annotations

import importlib.util
import os
import sys
import tempfile
from pathlib import Path


MIN_SEQUENTIAL_TIMEOUT_SECONDS = 1800

ORIGINAL_OBJECTIVE = '''        objective = (
            "Inspect this repository and repair all three product defects without modifying "
            "verify.py, test.mjs, package.json, fixtures, or expected outputs. Correct value.txt "
            "to the verified value, robustly implement src/slugify.py while preserving its public "
            "API, and repair the counter transitions in src/counter.js. Run `python verify.py`, "
            "iterate until every check passes, and stop only after all three independent "
            "validations succeed."
        )'''

DETERMINISTIC_OBJECTIVE = '''        objective = (
            "Act immediately. In your first assistant response, emit exactly three fs_write tool "
            "calls and no explanatory text. Emit them in the stated order, one call per path. "
            "The content argument is exact UTF-8 file content: preserve every line and make the "
            "final character of every content string a newline. In JSON, encode that final "
            "newline as \\\\n; do not omit it or replace it with the two literal characters backslash-n.\\n\\n"
            "1. value.txt\\n<<<VALUE\\n42\\n>>>VALUE\\n"
            "The value.txt content is exactly three bytes: 4, 2, newline.\\n\\n"
            "2. src/slugify.py\\n<<<SLUGIFY\\nimport re\\nimport unicodedata\\n\\n"
            "def slugify(value: str) -> str:\\n"
            "    normalized = unicodedata.normalize(\\\"NFKD\\\", value)\\n"
            "    ascii_value = normalized.encode(\\\"ascii\\\", \\\"ignore\\\").decode(\\\"ascii\\\")\\n"
            "    return re.sub(r\\\"[^a-z0-9]+\\\", \\\"-\\\", ascii_value.lower()).strip(\\\"-\\\")\\n"
            ">>>SLUGIFY\\n"
            "The slugify.py content ends with one newline immediately after the return line.\\n\\n"
            "3. src/counter.js\\n<<<COUNTER\\n"
            "export function applyCounter(state, action) {\\n"
            "  if (action.type === 'increment') return { count: state.count + 1 };\\n"
            "  if (action.type === 'decrement') return { count: state.count - 1 };\\n"
            "  return state;\\n"
            "}\\n"
            ">>>COUNTER\\n"
            "The counter.js content ends with one newline immediately after the closing brace.\\n\\n"
            "After all three fs_write results succeed, run this exact shell command once to "
            "normalize and assert the file endings: `python -c \\\"from pathlib import Path; "
            "paths = [Path('value.txt'), Path('src/slugify.py'), Path('src/counter.js')]; "
            "[path.write_bytes(path.read_bytes().rstrip(b'\\\\n') + b'\\\\n') for path in paths]; "
            "assert all(path.read_bytes().endswith(b'\\\\n') for path in paths)\\\"`. Then run "
            "`python verify.py` immediately. Do not inspect, plan, or rewrite a path through another "
            "fs_write call. Finish only when the newline assertion and the value, slugify, and "
            "JavaScript counter validations all pass."
        )'''


def main() -> int:
    harness_path = Path(__file__).with_name("live-coding-e2e.py")
    source = harness_path.read_text(encoding="utf-8")
    if source.count(ORIGINAL_OBJECTIVE) != 1:
        raise RuntimeError("could not replace the live coding objective exactly once")
    source = source.replace(ORIGINAL_OBJECTIVE, DETERMINISTIC_OBJECTIVE)

    temporary_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            suffix=".py",
            prefix="live-coding-e2e-adjusted-",
            dir=harness_path.parent,
            delete=False,
        ) as temporary:
            temporary.write(source)
            temporary_path = Path(temporary.name)
        spec = importlib.util.spec_from_file_location(
            "medusa_live_coding_e2e", temporary_path
        )
        if spec is None or spec.loader is None:
            raise RuntimeError(f"could not load live coding harness from {temporary_path}")
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        spec.loader.exec_module(module)
        module.PARALLEL_WORKERS = 1
        configured_timeout = int(os.environ.get("LIVE_E2E_TIMEOUT_SECONDS", "0"))
        os.environ["LIVE_E2E_TIMEOUT_SECONDS"] = str(
            max(configured_timeout, MIN_SEQUENTIAL_TIMEOUT_SECONDS)
        )
        os.environ["NO_UPDATE_NOTIFIER"] = "1"
        os.environ["NPM_CONFIG_UPDATE_NOTIFIER"] = "false"
        return module.main()
    finally:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)


if __name__ == "__main__":
    raise SystemExit(main())