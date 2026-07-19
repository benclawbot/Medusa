#!/usr/bin/env python3
"""Measure and compare the Rust workspace dependency graph using Cargo metadata."""

from __future__ import annotations

import argparse
import collections
import json
import pathlib
import subprocess
import sys
from typing import Any


def run(command: list[str], cwd: pathlib.Path) -> str:
    completed = subprocess.run(
        command,
        cwd=cwd,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if completed.returncode != 0:
        print(completed.stderr, file=sys.stderr)
        raise SystemExit(completed.returncode)
    return completed.stdout


def dependency_kind(dependency: dict[str, Any]) -> str:
    kind = dependency.get("kind")
    return "normal" if kind is None else str(kind)


def measure(root: pathlib.Path) -> dict[str, Any]:
    root = root.resolve()
    metadata = json.loads(
        run(
            [
                "cargo",
                "metadata",
                "--format-version",
                "1",
                "--locked",
                "--all-features",
            ],
            root,
        )
    )
    workspace_members = set(metadata["workspace_members"])
    resolved_ids = {node["id"] for node in metadata["resolve"]["nodes"]}
    packages_by_id = {package["id"]: package for package in metadata["packages"]}
    workspace_packages = [packages_by_id[member] for member in workspace_members]
    resolved_packages = [packages_by_id[package_id] for package_id in resolved_ids]

    direct_edges = collections.Counter()
    external_edges = collections.Counter()
    external_by_crate: dict[str, list[str]] = {}
    for package in workspace_packages:
        external_names: list[str] = []
        for dependency in package["dependencies"]:
            kind = dependency_kind(dependency)
            direct_edges[kind] += 1
            if dependency.get("source") is not None:
                external_edges[kind] += 1
                external_names.append(
                    f"{dependency['name']} ({kind})"
                    + (f" [{dependency['target']}]" if dependency.get("target") else "")
                )
        external_by_crate[package["name"]] = sorted(external_names)

    versions_by_name: dict[str, set[str]] = collections.defaultdict(set)
    for package in resolved_packages:
        versions_by_name[package["name"]].add(package["version"])
    duplicates = {
        name: sorted(versions)
        for name, versions in versions_by_name.items()
        if len(versions) > 1
    }

    resolved_nodes = metadata["resolve"]["nodes"]
    lock_path = root / "Cargo.lock"
    lock_text = lock_path.read_text(encoding="utf-8")
    duplicate_tree = run(["cargo", "tree", "-d", "--locked"], root)

    return {
        "workspace_packages": len(workspace_packages),
        "direct_edges": {
            "total": sum(direct_edges.values()),
            "normal": direct_edges["normal"],
            "build": direct_edges["build"],
            "dev": direct_edges["dev"],
        },
        "external_direct_edges": {
            "total": sum(external_edges.values()),
            "normal": external_edges["normal"],
            "build": external_edges["build"],
            "dev": external_edges["dev"],
        },
        "external_direct_by_crate": dict(sorted(external_by_crate.items())),
        "locked_packages": lock_text.count("[[package]]"),
        "resolved_packages": len(resolved_packages),
        "registry_packages": sum(
            1 for package in resolved_packages if package.get("source") is not None
        ),
        "duplicate_package_names": len(duplicates),
        "duplicate_extra_versions": sum(len(versions) - 1 for versions in duplicates.values()),
        "duplicate_versions": dict(sorted(duplicates.items())),
        "enabled_feature_selections": sum(len(node.get("features", [])) for node in resolved_nodes),
        "packages_with_enabled_features": sum(
            1 for node in resolved_nodes if node.get("features")
        ),
        "cargo_tree_duplicates": duplicate_tree,
    }


def write_json(path: pathlib.Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def compare(base: dict[str, Any], current: dict[str, Any]) -> str:
    rows = [
        ("Workspace packages", "workspace_packages"),
        ("Direct dependency edges", ("direct_edges", "total")),
        ("Normal direct edges", ("direct_edges", "normal")),
        ("Development direct edges", ("direct_edges", "dev")),
        ("External direct edges", ("external_direct_edges", "total")),
        ("Locked packages", "locked_packages"),
        ("Resolved packages", "resolved_packages"),
        ("Registry packages", "registry_packages"),
        ("Duplicate package names", "duplicate_package_names"),
        ("Duplicate extra versions", "duplicate_extra_versions"),
        ("Enabled feature selections", "enabled_feature_selections"),
    ]

    def value(payload: dict[str, Any], key: str | tuple[str, str]) -> int:
        if isinstance(key, tuple):
            return int(payload[key[0]][key[1]])
        return int(payload[key])

    lines = [
        "# Dependency metrics comparison",
        "",
        "| Metric | Base | Current | Delta |",
        "|---|---:|---:|---:|",
    ]
    for label, key in rows:
        before = value(base, key)
        after = value(current, key)
        lines.append(f"| {label} | {before} | {after} | {after - before:+d} |")

    removed_edges: list[str] = []
    added_edges: list[str] = []
    crate_names = sorted(
        set(base["external_direct_by_crate"]) | set(current["external_direct_by_crate"])
    )
    for crate in crate_names:
        before = set(base["external_direct_by_crate"].get(crate, []))
        after = set(current["external_direct_by_crate"].get(crate, []))
        removed_edges.extend(f"`{crate}` → `{edge}`" for edge in sorted(before - after))
        added_edges.extend(f"`{crate}` → `{edge}`" for edge in sorted(after - before))

    lines.extend(["", "## Direct external edge changes", ""])
    lines.append("Removed: " + (", ".join(removed_edges) if removed_edges else "none"))
    lines.append("")
    lines.append("Added: " + (", ".join(added_edges) if added_edges else "none"))
    lines.append("")
    lines.append(
        "Resolved-package and feature counts are exact Cargo metadata measurements, not estimated build-time claims."
    )
    return "\n".join(lines) + "\n"


def main() -> None:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)

    measure_parser = subparsers.add_parser("measure")
    measure_parser.add_argument("--root", type=pathlib.Path, required=True)
    measure_parser.add_argument("--output", type=pathlib.Path, required=True)

    compare_parser = subparsers.add_parser("compare")
    compare_parser.add_argument("--base", type=pathlib.Path, required=True)
    compare_parser.add_argument("--current", type=pathlib.Path, required=True)
    compare_parser.add_argument("--output", type=pathlib.Path, required=True)

    arguments = parser.parse_args()
    if arguments.command == "measure":
        write_json(arguments.output, measure(arguments.root))
        return

    base = json.loads(arguments.base.read_text(encoding="utf-8"))
    current = json.loads(arguments.current.read_text(encoding="utf-8"))
    report = compare(base, current)
    arguments.output.write_text(report, encoding="utf-8")
    print(report)


if __name__ == "__main__":
    main()
