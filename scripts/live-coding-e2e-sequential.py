#!/usr/bin/env python3
"""Run the live coding harness with deterministic delegated-worker instructions."""

from __future__ import annotations

import importlib.util
import sys
import tempfile
from pathlib import Path


ORIGINAL_OBJECTIVE = '''        objective = (
            "Inspect this repository and repair all three product defects without modifying "
            "verify.py, test.mjs, package.json, fixtures, or expected outputs. Correct value.txt "
            "to the verified value, robustly implement src/slugify.py while preserving its public "
            "API, and repair the counter transitions in src/counter.js. Run `python verify.py`, "
            "iterate until every check passes, and stop only after all three independent "
            "validations succeed."
        )'''

DETERMINISTIC_OBJECTIVE = '''        objective = (
            "Use fs_write exactly once for each of the three writable paths below, in the stated "
            "order. Completion requires three successful writes. Replace each complete file with "
            "the exact UTF-8 payload shown, including its final newline.\\n\\n"
            "1. value.txt\\n<<<VALUE\\n42\\n>>>VALUE\\n\\n"
            "2. src/slugify.py\\n<<<SLUGIFY\\nimport re\\nimport unicodedata\\n\\n"
            "def slugify(value: str) -> str:\\n"
            "    normalized = unicodedata.normalize(\\\"NFKD\\\", value)\\n"
            "    ascii_value = normalized.encode(\\\"ascii\\\", \\\"ignore\\\").decode(\\\"ascii\\\")\\n"
            "    return re.sub(r\\\"[^a-z0-9]+\\\", \\\"-\\\", ascii_value.lower()).strip(\\\"-\\\")\\n"
            ">>>SLUGIFY\\n\\n"
            "3. src/counter.js\\n<<<COUNTER\\n"
            "export function applyCounter(state, action) {\\n"
            "  if (action.type === 'increment') return { count: state.count + 1 };\\n"
            "  if (action.type === 'decrement') return { count: state.count - 1 };\\n"
            "  return state;\\n"
            "}\\n"
            ">>>COUNTER\\n\\n"
            "After all three fs_write calls succeed, run `python verify.py`. Finish only when the "
            "value, slugify, and JavaScript counter validations all pass."
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
        return module.main()
    finally:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)


if __name__ == "__main__":
    raise SystemExit(main())
