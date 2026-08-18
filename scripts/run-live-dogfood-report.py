#!/usr/bin/env python3
"""Validate dogfood evidence and render the provider/model actually exercised."""

from __future__ import annotations

import argparse
import importlib.util
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def evidence_route(summaries: list[dict[str, object]]) -> tuple[str, str, list[str]]:
    providers = {str(summary.get("provider")) for summary in summaries}
    models = {str(summary.get("model")) for summary in summaries}
    errors: list[str] = []
    if len(providers) != 1 or "None" in providers:
        errors.append(f"evidence does not share one provider: {sorted(providers)}")
    if len(models) != 1 or "None" in models:
        errors.append(f"evidence does not share one model: {sorted(models)}")
    provider = next(iter(providers), "unknown")
    model = next(iter(models), "unknown")
    return provider, model, errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--json", type=Path, required=True)
    parser.add_argument("--markdown", type=Path, required=True)
    args = parser.parse_args()

    report = load("live_dogfood_report", ROOT / "live-dogfood-report.py")
    summaries = report.load_summaries(args.input)
    errors = report.validate(summaries)
    provider, model, route_errors = evidence_route(summaries)
    errors.extend(route_errors)

    payload = {
        "schema_version": 1,
        "result": "passed" if not errors else "failed",
        "errors": errors,
        "provider": provider,
        "model": model,
        "platforms": summaries,
    }
    args.json.parent.mkdir(parents=True, exist_ok=True)
    args.json.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    markdown = report.render_markdown(summaries, errors)
    markdown = markdown.replace("minimax / MiniMax-M3", f"{provider} / {model}")
    markdown = markdown.replace(
        "It exercises MiniMax-M3; other provider routes retain their own compatibility and authentication gates.",
        f"It exercises {model}; other provider routes retain their own compatibility and authentication gates.",
    )
    args.markdown.write_text(markdown, encoding="utf-8")
    return 1 if errors else 0


if __name__ == "__main__":
    raise SystemExit(main())
