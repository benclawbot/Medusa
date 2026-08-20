#!/usr/bin/env python3
"""Regression fixtures for repository-specific engineering-policy obligations."""
from __future__ import annotations

import importlib.util
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts/engineering-policy.py"
SPEC = importlib.util.spec_from_file_location("engineering_policy", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"cannot load {MODULE_PATH}")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def resolved(path: str) -> dict[str, object]:
    policy = MODULE.load_policy(ROOT / ".github/engineering-policy.json", ROOT)
    return MODULE.resolve(policy, [path])


def rule_ids(report: dict[str, object]) -> set[str]:
    return {rule["id"] for rule in report["triggered_rules"]}


def checks(report: dict[str, object]) -> set[str]:
    return set(report["required_checks"])


def main() -> int:
    docs = resolved("docs/guide.md")
    assert "generated-documentation-inventory" in rule_ids(docs)
    assert "documentation-inventory" in checks(docs)
    assert not docs["protected_change"]

    provider = resolved("docs/provider-support.json")
    assert "provider-support-source-of-truth" in rule_ids(provider)
    assert "provider-support-sync" in checks(provider)

    claims = resolved("docs/CAPABILITY-CLAIMS.json")
    assert "capability-claim-synchronization" in rule_ids(claims)
    assert "capability-claims-sync" in checks(claims)

    authority = resolved("docs/architecture/baseline.json")
    assert "canonical-truth-authorities" in rule_ids(authority)
    assert {"canonical-truth-authority", "evidence-authority", "architecture-policy"} <= checks(authority)
    assert authority["protected_change"]

    runtime = resolved("crates/medusa-runtime/src/lib.rs")
    assert "canonical-truth-authorities" in rule_ids(runtime)
    assert "capability-claim-synchronization" in rule_ids(runtime)
    assert runtime["protected_change"]

    first = resolved("docs/provider-support.json")
    second = resolved("docs/provider-support.json")
    assert first == second

    print("engineering policy contract fixtures passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
