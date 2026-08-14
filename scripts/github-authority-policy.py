#!/usr/bin/env python3
"""Enforce one production GitHub operation authority.

The shipped CLI must route GitHub operation requests directly through medusa-github.
The retired medusa-external-github adapter must not re-enter the workspace, CLI graph,
or architecture evidence.
"""
from __future__ import annotations

import json
import pathlib
import sys


def fail(errors: list[str]) -> int:
    for error in errors:
        print(f"github-authority-policy: {error}", file=sys.stderr)
    return 1 if errors else 0


def check(root: pathlib.Path) -> int:
    errors: list[str] = []
    retired = "medusa-external-github"

    workspace = (root / "Cargo.toml").read_text(encoding="utf-8")
    cli_manifest = (root / "crates/medusa-cli/Cargo.toml").read_text(encoding="utf-8")
    cli_entrypoint = (root / "crates/medusa-cli/src/github_operation.rs").read_text(
        encoding="utf-8"
    )
    owners = json.loads((root / "docs/architecture/owners.json").read_text(encoding="utf-8"))
    baseline = json.loads(
        (root / "docs/architecture/baseline.json").read_text(encoding="utf-8")
    )
    index = (root / "docs/architecture/INDEX.md").read_text(encoding="utf-8")

    if retired in workspace:
        errors.append("retired medusa-external-github remains a workspace member")
    if retired in cli_manifest:
        errors.append("CLI retains the retired medusa-external-github dependency")
    if "medusa-github" not in cli_manifest:
        errors.append("CLI does not depend on medusa-github")
    if "use medusa_github::" not in cli_entrypoint:
        errors.append("production medusa-github-operation entrypoint does not import medusa-github")
    if "medusa_external_github" in cli_entrypoint:
        errors.append("production CLI still calls the retired adapter")
    if (root / "crates/medusa-external-github").exists():
        errors.append("retired medusa-external-github crate directory still exists")
    if retired in owners.get("owners", {}):
        errors.append("retired adapter still has an architecture owner")

    rust_crates = baseline["components"]["rust_crates"]
    if retired in rust_crates:
        errors.append("retired adapter remains in the certified crate inventory")
    integration_owners = baseline["components"]["owner_groups"]["integrations"]
    if retired in integration_owners:
        errors.append("retired adapter remains in the integrations owner group")
    github_paths = baseline["capability_paths"]["github-service"]
    if github_paths != ["crates/medusa-github"]:
        errors.append(
            "github-service capability path must contain only crates/medusa-github"
        )
    deployment = next(
        (row for row in baseline["deployment_modes"] if row[0] == "github-operations"),
        None,
    )
    if deployment is None or deployment[2] != "crates/medusa-github":
        errors.append("GitHub deployment mode is not bound to crates/medusa-github")
    if retired in index:
        errors.append("architecture index still names the retired adapter")

    return fail(errors)


def main() -> int:
    root = pathlib.Path(__file__).resolve().parents[1]
    return check(root)


if __name__ == "__main__":
    raise SystemExit(main())
