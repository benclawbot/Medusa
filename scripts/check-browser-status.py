#!/usr/bin/env python3
"""Fail closed when browser capability authorities contradict each other."""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path


class BrowserStatusError(RuntimeError):
    pass


def read(root: Path, relative: str) -> str:
    path = root / relative
    try:
        value = path.read_text(encoding="utf-8")
    except FileNotFoundError as exc:
        raise BrowserStatusError(f"missing browser status authority: {relative}") from exc
    if not value.strip():
        raise BrowserStatusError(f"empty browser status authority: {relative}")
    return value


def validate(root: Path) -> None:
    baseline = json.loads(read(root, "docs/architecture/baseline.json"))
    rows = baseline.get("capabilities", [])
    browser = [row for row in rows if isinstance(row, list) and row and row[0] == "browser-tools"]
    if len(browser) != 1 or len(browser[0]) != 6:
        raise BrowserStatusError("baseline must contain exactly one browser-tools capability row")
    _, product, certification, disposition, dispatcher, gaps = browser[0]
    if (product, certification, disposition) != ("preview", "certified-production", "preserve"):
        raise BrowserStatusError(
            "browser baseline must be preview/certified-production/preserve; "
            f"got {product}/{certification}/{disposition}"
        )
    if dispatcher != "medusa-agent::ToolManager -> medusa-browserd" or gaps != []:
        raise BrowserStatusError("browser baseline dispatcher/evidence does not match certified preview authority")

    paths = baseline.get("capability_paths", {}).get("browser-tools", [])
    required_paths = {
        "crates/medusa-capabilities",
        "crates/medusa-agent/src/tools",
        "crates/medusa-browser-client",
        "crates/medusa-browserd",
        "crates/medusa-agent/tests/browser_dispatch.rs",
        ".github/workflows/browser-dispatch-certification.yml",
    }
    if not required_paths.issubset(set(paths)):
        raise BrowserStatusError("browser capability_paths lost a certified dispatcher or evidence path")

    index = read(root, "docs/architecture/INDEX.md")
    readme = read(root, "README.md")
    config = read(root, "docs/CONFIGURATION.md")
    adr = read(root, "docs/architecture/decisions/0009-browser-preview-certification.md")

    forbidden = (
        "model-executable browser actions remain withheld",
        "Model-executable browser actions remain quarantined",
        "| Browser tools | withheld | quarantined |",
        "no executable projection until dispatcher",
    )
    combined = readme + "\n" + index
    stale = [claim for claim in forbidden if claim in combined]
    if stale:
        raise BrowserStatusError(f"human-facing browser status contradicts runtime authority: {stale}")

    required_index = (
        "| Browser tools | preview | certified-production |",
        "readiness-gated",
        "explicit opt-in",
        "medusa-agent::ToolManager",
        "medusa-browserd",
        "0009-browser-preview-certification.md",
    )
    missing = [claim for claim in required_index if claim not in index]
    if missing:
        raise BrowserStatusError(f"architecture index is missing browser status authority: {missing}")

    required_readme = (
        "browser actions are readiness-gated preview",
        "MEDUSA_BROWSER_ENABLED",
        "MEDUSA_BROWSER_PATH",
        "MEDUSA_BROWSER_VERIFY_URL",
    )
    missing = [claim for claim in required_readme if claim not in readme]
    if missing:
        raise BrowserStatusError(f"README is missing browser preview/prerequisite wording: {missing}")

    required_config = (
        "MEDUSA_BROWSER_ENABLED",
        "MEDUSA_BROWSER_PATH",
        "MEDUSA_BROWSER_VERIFY_URL",
        "MEDUSA_BROWSER_TIMEOUT_MS",
        "browser_evaluate",
        "readiness-gated preview",
    )
    missing = [claim for claim in required_config if claim not in config]
    if missing:
        raise BrowserStatusError(f"configuration guide is missing browser contract details: {missing}")

    required_adr = (
        "product status `preview`",
        "architecture status `certified-production`",
        "MEDUSA_BROWSER_ENABLED=true",
        "MEDUSA_BROWSER_TIMEOUT_MS",
        "browser_evaluate",
    )
    missing = [claim for claim in required_adr if claim not in adr]
    if missing:
        raise BrowserStatusError(f"browser ADR is incomplete: {missing}")

    registry = read(root, "crates/medusa-capabilities/src/registry.rs")
    for symbol in ("browser_capability_state", "MEDUSA_BROWSER_VERIFY_URL", "Capability::Browser"):
        if symbol not in registry:
            raise BrowserStatusError(f"runtime registry lost browser readiness authority: {symbol}")
    if not re.search(r"browser.*explicit.*disabled", registry, re.IGNORECASE | re.DOTALL):
        raise BrowserStatusError("runtime registry no longer documents explicit browser enablement")

    decisions = root / "docs/architecture/decisions"
    if not decisions.joinpath("0009-browser-preview-certification.md").is_file():
        raise BrowserStatusError("accepted browser ADR is missing")


def main() -> int:
    root = Path(sys.argv[1] if len(sys.argv) > 1 else ".").resolve()
    try:
        validate(root)
    except (BrowserStatusError, json.JSONDecodeError) as exc:
        print(f"browser-status-error: {exc}", file=sys.stderr)
        return 1
    print("browser-status-ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
