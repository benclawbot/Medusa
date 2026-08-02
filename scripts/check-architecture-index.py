#!/usr/bin/env python3
"""Validate the Architecture v2 living index against the repository."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any

import tomllib

VALID_DISPOSITIONS = {"preserve", "adapt", "replace", "quarantine", "delete"}
VALID_CERTIFICATIONS = {
    "certified-production",
    "legacy-uncertified",
    "quarantined",
    "preview",
    "experimental",
    "design-only",
    "deprecated",
}
REQUIRED_MIGRATION_ISSUES = set(range(646, 656))
REQUIRED_FIXTURES = {
    "browser-dispatch-unreachable",
    "integration-precedes-parent-review",
    "isolated-verification-drops-changed-paths",
    "provider-capability-mismatch",
}
REQUIRED_PR_TEXT = {
    "## Architecture impact declaration",
    "No architecture impact",
    "Authority or source of truth",
    "Versioned contracts or schemas",
    "Trust/security boundary",
    "Dependency direction",
    "Production entrypoint or deployment mode",
    "Capability lifecycle, readiness, permissions, or dispatcher",
    "Legacy deletion target",
}
REQUIRED_CODEOWNERS = {
    "/docs/architecture/",
    "/scripts/check-architecture-index.py",
    "/scripts/architecture-conformance.py",
    "/crates/medusa-runtime/",
    "/crates/medusa-agent/",
    "/crates/medusa-provider/",
    "/crates/medusa-process-containment/",
    "/crates/medusa-update/",
}
REQUIRED_INDEX_SECTIONS = {
    "## Phase 0 feature freeze",
    "## Current v1 map",
    "## Target v2 map",
    "## Capability certification",
    "## Source-of-truth matrix",
    "## Dataflows",
    "## Trust boundaries",
    "## Known-failure compatibility fixtures",
    "## Extension procedure",
    "## Migration and deletion",
}
MARKDOWN_LINK = re.compile(r"\[[^\]]+\]\(([^)]+)\)")
CRATE_REFERENCE = re.compile(r"(?<![A-Za-z0-9_-])crates/(medusa-[A-Za-z0-9_-]+)")


class ArchitectureIndexError(RuntimeError):
    """Raised when the living architecture index drifts from the repository."""


def read_text(root: Path, relative: str) -> str:
    path = root / relative
    try:
        text = path.read_text(encoding="utf-8")
    except FileNotFoundError as exc:
        raise ArchitectureIndexError(f"missing required path: {relative}") from exc
    if not text.strip():
        raise ArchitectureIndexError(f"empty required path: {relative}")
    return text


def load_json(root: Path, relative: str) -> dict[str, Any]:
    try:
        value = json.loads(read_text(root, relative))
    except json.JSONDecodeError as exc:
        raise ArchitectureIndexError(f"invalid JSON in {relative}: {exc}") from exc
    if not isinstance(value, dict):
        raise ArchitectureIndexError(f"{relative} must contain a JSON object")
    return value


def require_unique(values: list[str], context: str) -> None:
    duplicates = sorted({value for value in values if values.count(value) > 1})
    if duplicates:
        raise ArchitectureIndexError(f"duplicate {context}: {duplicates}")


def validate_workspace(root: Path, manifest: dict[str, Any]) -> set[str]:
    cargo = tomllib.loads(read_text(root, "Cargo.toml"))
    members = cargo.get("workspace", {}).get("members")
    if not isinstance(members, list):
        raise ArchitectureIndexError("Cargo.toml workspace.members must be a list")
    workspace_crates = {
        member.removeprefix("crates/")
        for member in members
        if isinstance(member, str) and member.startswith("crates/")
    }
    indexed = manifest.get("components", {}).get("rust_crates")
    if not isinstance(indexed, dict):
        raise ArchitectureIndexError("components.rust_crates must be an object")
    indexed_crates = set(indexed)
    if workspace_crates != indexed_crates:
        missing = sorted(workspace_crates - indexed_crates)
        unknown = sorted(indexed_crates - workspace_crates)
        raise ArchitectureIndexError(
            f"workspace/index crate mismatch; missing={missing}, unknown={unknown}"
        )
    invalid = sorted(
        f"{name}:{disposition}"
        for name, disposition in indexed.items()
        if disposition not in VALID_DISPOSITIONS
    )
    if invalid:
        raise ArchitectureIndexError(f"invalid component dispositions: {invalid}")
    for name in indexed:
        if not (root / "crates" / name / "Cargo.toml").is_file():
            raise ArchitectureIndexError(f"indexed crate has no Cargo.toml: {name}")

    owner_groups = manifest.get("components", {}).get("owner_groups")
    if not isinstance(owner_groups, dict) or not owner_groups:
        raise ArchitectureIndexError("components.owner_groups must be a non-empty object")
    owned = {
        item
        for values in owner_groups.values()
        if isinstance(values, list)
        for item in values
        if isinstance(item, str)
    }
    unowned = sorted(workspace_crates - owned)
    unknown_owned = sorted(owned - workspace_crates)
    if unowned or unknown_owned:
        raise ArchitectureIndexError(
            f"owner-group drift; unowned={unowned}, unknown={unknown_owned}"
        )
    return workspace_crates


def validate_components_and_paths(root: Path, manifest: dict[str, Any]) -> None:
    non_crate = manifest.get("components", {}).get("non_crate")
    if not isinstance(non_crate, list):
        raise ArchitectureIndexError("components.non_crate must be a list")
    identifiers: list[str] = []
    for row in non_crate:
        if not isinstance(row, list) or len(row) != 3:
            raise ArchitectureIndexError(f"invalid non-crate component row: {row!r}")
        component_id, relative, disposition = row
        if not all(isinstance(item, str) and item for item in row):
            raise ArchitectureIndexError(f"invalid non-crate component values: {row!r}")
        if disposition not in VALID_DISPOSITIONS:
            raise ArchitectureIndexError(
                f"invalid non-crate disposition {component_id}:{disposition}"
            )
        if not (root / relative).exists():
            raise ArchitectureIndexError(
                f"indexed non-crate component path does not exist: {relative}"
            )
        identifiers.append(component_id)
    require_unique(identifiers, "non-crate component id")


def validate_deployment_modes(root: Path, manifest: dict[str, Any]) -> None:
    modes = manifest.get("deployment_modes")
    if not isinstance(modes, list) or not modes:
        raise ArchitectureIndexError("deployment_modes must be a non-empty list")
    identifiers: list[str] = []
    for row in modes:
        if not isinstance(row, list) or len(row) != 4:
            raise ArchitectureIndexError(f"invalid deployment mode row: {row!r}")
        mode_id, entrypoint, implementation, shared_path = row
        if not all(isinstance(item, str) and item.strip() for item in row):
            raise ArchitectureIndexError(f"empty deployment mode value: {row!r}")
        if not (root / implementation).exists():
            raise ArchitectureIndexError(
                f"documented production entrypoint lacks implementation: {mode_id} -> {implementation}"
            )
        identifiers.append(mode_id)
    require_unique(identifiers, "deployment mode id")


def validate_capabilities(root: Path, manifest: dict[str, Any]) -> None:
    capabilities = manifest.get("capabilities")
    paths = manifest.get("capability_paths")
    if not isinstance(capabilities, list) or not capabilities:
        raise ArchitectureIndexError("capabilities must be a non-empty list")
    if not isinstance(paths, dict):
        raise ArchitectureIndexError("capability_paths must be an object")
    identifiers: list[str] = []
    for row in capabilities:
        if not isinstance(row, list) or len(row) != 6:
            raise ArchitectureIndexError(f"invalid capability row: {row!r}")
        capability_id, legacy, certification, disposition, dispatcher, gaps = row
        if not all(isinstance(item, str) and item for item in row[:5]):
            raise ArchitectureIndexError(f"invalid capability values: {row!r}")
        if certification not in VALID_CERTIFICATIONS:
            raise ArchitectureIndexError(
                f"invalid v2 certification for {capability_id}: {certification}"
            )
        if disposition not in VALID_DISPOSITIONS:
            raise ArchitectureIndexError(
                f"invalid capability disposition for {capability_id}: {disposition}"
            )
        if not isinstance(gaps, list):
            raise ArchitectureIndexError(f"capability gaps must be a list: {capability_id}")
        capability_paths = paths.get(capability_id)
        if not isinstance(capability_paths, list) or not capability_paths:
            raise ArchitectureIndexError(
                f"capability {capability_id} has no documented implementation paths"
            )
        for relative in capability_paths:
            if not isinstance(relative, str) or not (root / relative).exists():
                raise ArchitectureIndexError(
                    f"capability {capability_id} references missing implementation: {relative!r}"
                )
        identifiers.append(capability_id)
    require_unique(identifiers, "capability id")
    if set(identifiers) != set(paths):
        raise ArchitectureIndexError("capability_paths keys must exactly match capability ids")


def validate_sources_and_lifecycle(manifest: dict[str, Any]) -> None:
    sources = manifest.get("sources_of_truth")
    if not isinstance(sources, list) or not sources:
        raise ArchitectureIndexError("sources_of_truth must be a non-empty list")
    concerns: list[str] = []
    authorities: list[str] = []
    for row in sources:
        if not isinstance(row, list) or len(row) != 5:
            raise ArchitectureIndexError(f"invalid source-of-truth row: {row!r}")
        concern, authority, duplicates, target, invariant = row
        if not all(isinstance(item, str) and item for item in (concern, authority, target, invariant)):
            raise ArchitectureIndexError(f"invalid source-of-truth values: {row!r}")
        if not isinstance(duplicates, list):
            raise ArchitectureIndexError(f"legacy duplicates must be a list: {concern}")
        concerns.append(concern)
        authorities.append(authority)
    require_unique(concerns, "source-of-truth concern")
    require_unique(authorities, "current authority")

    machines = manifest.get("state_machines")
    if not isinstance(machines, list) or not machines:
        raise ArchitectureIndexError("state_machines must be a non-empty list")
    machine_ids = [row[0] for row in machines if isinstance(row, list) and len(row) == 3]
    if len(machine_ids) != len(machines):
        raise ArchitectureIndexError("every state machine must have id, states, and invariant")
    require_unique(machine_ids, "state machine id")


def validate_fixtures_and_migration(manifest: dict[str, Any]) -> None:
    fixtures = manifest.get("known_failure_fixtures")
    if not isinstance(fixtures, list):
        raise ArchitectureIndexError("known_failure_fixtures must be a list")
    ids: list[str] = []
    for row in fixtures:
        if not isinstance(row, list) or len(row) != 5:
            raise ArchitectureIndexError(f"invalid known-failure fixture: {row!r}")
        fixture_id, issue, desired, probe, remove_when = row
        if not isinstance(fixture_id, str) or not isinstance(issue, int):
            raise ArchitectureIndexError(f"invalid fixture identity: {row!r}")
        if desired is not False:
            raise ArchitectureIndexError(
                f"known-failure fixture must explicitly set desired=false: {fixture_id}"
            )
        if not isinstance(probe, str) or not isinstance(remove_when, str):
            raise ArchitectureIndexError(f"fixture requires probe and removal rule: {fixture_id}")
        ids.append(fixture_id)
    require_unique(ids, "known-failure fixture id")
    missing = sorted(REQUIRED_FIXTURES - set(ids))
    if missing:
        raise ArchitectureIndexError(f"missing required known-failure fixtures: {missing}")

    migration = manifest.get("migration")
    if not isinstance(migration, list):
        raise ArchitectureIndexError("migration must be a list")
    issues: list[int] = []
    for row in migration:
        if not isinstance(row, list) or len(row) != 7:
            raise ArchitectureIndexError(f"invalid migration row: {row!r}")
        issue, phase, goal, owner, contracts, consumers, deletion_target = row
        if not isinstance(issue, int) or not all(
            isinstance(item, str) and item for item in (phase, goal, owner, deletion_target)
        ):
            raise ArchitectureIndexError(f"invalid migration values: {row!r}")
        if not isinstance(contracts, list) or not contracts or not isinstance(consumers, list) or not consumers:
            raise ArchitectureIndexError(f"migration lacks contracts or consumers: #{issue}")
        issues.append(issue)
    require_unique([str(issue) for issue in issues], "migration issue")
    missing_issues = sorted(REQUIRED_MIGRATION_ISSUES - set(issues))
    if missing_issues:
        raise ArchitectureIndexError(f"migration graph is missing issues: {missing_issues}")


def validate_dependency_policy(root: Path, manifest: dict[str, Any]) -> None:
    edges = manifest.get("dependency_policy", {}).get("forbidden_edges")
    if not isinstance(edges, list) or not edges:
        raise ArchitectureIndexError("dependency_policy.forbidden_edges must be non-empty")
    for row in edges:
        if not isinstance(row, list) or len(row) != 2:
            raise ArchitectureIndexError(f"invalid forbidden dependency edge: {row!r}")
        source, target = row
        cargo_path = root / source / "Cargo.toml"
        if not cargo_path.is_file():
            raise ArchitectureIndexError(f"forbidden-edge source does not exist: {source}")
        cargo = cargo_path.read_text(encoding="utf-8")
        package = target.removeprefix("crates/")
        if re.search(rf"(?m)^\s*{re.escape(package)}\s*=", cargo):
            raise ArchitectureIndexError(f"forbidden dependency present: {source} -> {package}")


def validate_governance(root: Path, manifest: dict[str, Any]) -> None:
    governance = manifest.get("governance")
    if not isinstance(governance, dict):
        raise ArchitectureIndexError("governance must be an object")
    for label, relative in governance.items():
        if not isinstance(relative, str) or not (root / relative).exists():
            raise ArchitectureIndexError(f"governance path missing for {label}: {relative!r}")

    index = read_text(root, governance["index"])
    for section in REQUIRED_INDEX_SECTIONS:
        if section not in index:
            raise ArchitectureIndexError(f"architecture index is missing section: {section}")
    base = (root / governance["index"]).parent
    for destination in MARKDOWN_LINK.findall(index):
        destination = destination.split("#", 1)[0]
        if not destination or "://" in destination or destination.startswith("mailto:"):
            continue
        if not (base / destination).resolve().exists():
            raise ArchitectureIndexError(f"stale architecture index link: {destination}")

    template = read_text(root, governance["pr_template"])
    missing_template = sorted(item for item in REQUIRED_PR_TEXT if item not in template)
    if missing_template:
        raise ArchitectureIndexError(
            f"pull request architecture declaration is incomplete: {missing_template}"
        )
    codeowners = read_text(root, governance["codeowners"])
    missing_owners = sorted(item for item in REQUIRED_CODEOWNERS if item not in codeowners)
    if missing_owners:
        raise ArchitectureIndexError(f"CODEOWNERS misses architecture boundaries: {missing_owners}")


def validate_documented_crates(root: Path, known_crates: set[str]) -> None:
    documents = [
        "docs/ARCHITECTURE.md",
        "docs/CONTRIBUTOR-ARCHITECTURE.md",
        "docs/architecture/INDEX.md",
        "docs/architecture/LEGACY-DELETION.md",
        "docs/architecture/RELEASE-POLICY.md",
    ]
    for relative in documents:
        text = read_text(root, relative)
        unknown = sorted(set(CRATE_REFERENCE.findall(text)) - known_crates)
        if unknown:
            raise ArchitectureIndexError(
                f"{relative} references unknown crates/components: {unknown}"
            )


def validate(root: Path, manifest_relative: str = "docs/architecture/baseline.json") -> None:
    manifest = load_json(root, manifest_relative)
    if manifest.get("schema_version") != 1:
        raise ArchitectureIndexError("unsupported architecture baseline schema_version")
    baseline = manifest.get("baseline", {})
    if baseline.get("issue") != 646 or baseline.get("parent_issue") != 645:
        raise ArchitectureIndexError("baseline must identify issues #645 and #646")
    freeze = baseline.get("feature_freeze", {})
    if freeze.get("active") is not True or not freeze.get("release_rule"):
        raise ArchitectureIndexError("phase-0 feature freeze and release rule must be active")

    known_crates = validate_workspace(root, manifest)
    validate_components_and_paths(root, manifest)
    validate_deployment_modes(root, manifest)
    validate_capabilities(root, manifest)
    validate_sources_and_lifecycle(manifest)
    validate_fixtures_and_migration(manifest)
    validate_dependency_policy(root, manifest)
    validate_governance(root, manifest)
    validate_documented_crates(root, known_crates)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path("."))
    parser.add_argument("--manifest", default="docs/architecture/baseline.json")
    args = parser.parse_args()
    try:
        validate(args.root.resolve(), args.manifest)
    except ArchitectureIndexError as exc:
        print(f"architecture-index-error: {exc}", file=sys.stderr)
        return 1
    print("architecture-index-ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
