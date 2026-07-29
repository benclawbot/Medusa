#!/usr/bin/env python3
"""Enforce shipped-crate reachability and reject hidden Rust dependencies."""
from __future__ import annotations

import argparse
import datetime as dt
import json
import pathlib
import re
import subprocess
import sys
from collections import defaultdict, deque
from typing import Any

KINDS = ("normal", "build", "dev")
SPLICE_PATTERNS = (
    re.compile(r"include!\s*\([^)]*(?:\.\./)+crates/[^)]*/src/", re.S),
    re.compile(r"(?:read_to_string|read|File::open)\s*\([^)]*(?:\.\./)+crates/[^)]*/src/", re.S),
    re.compile(r"(?:concat|extend_from_slice|push_str)\s*\([^)]*(?:\.\./)+crates/[^)]*/src/", re.S),
)
MOD_RE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;", re.M)
PATH_MOD_RE = re.compile(
    r"#\s*\[\s*path\s*=\s*\"([^\"]+)\"\s*\]\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+[A-Za-z_][A-Za-z0-9_]*\s*;",
    re.M,
)
INCLUDE_RE = re.compile(r"include!\s*\(\s*\"([^\"]+\.rs)\"\s*\)")
MANIFEST_INCLUDE_RE = re.compile(
    r'include!\s*\(\s*concat!\s*\(\s*env!\s*\(\s*"CARGO_MANIFEST_DIR"\s*\)\s*,\s*"/([^"]+\.rs)"\s*\)\s*\)',
    re.S,
)
BUILD_SOURCE_RE = re.compile(
    r'(?:read_to_string|read|File::open)\s*\(\s*"(src/[^"]+\.rs)"\s*\)',
    re.S,
)
RERUN_SOURCE_RE = re.compile(r'cargo:rerun-if-changed=(src/[^"\\]+\.rs)')
BOUND_MODULE_RE = re.compile(r'\(\s*"[^"]+"\s*,\s*"([^"]+\.rs)"\s*\)')


def run(command: list[str], cwd: pathlib.Path) -> str:
    result = subprocess.run(command, cwd=cwd, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or "command failed")
    return result.stdout


def load_metadata(root: pathlib.Path) -> dict[str, Any]:
    return json.loads(run(["cargo", "metadata", "--format-version", "1", "--locked", "--all-features"], root))


def dependency_graph(metadata: dict[str, Any]) -> tuple[dict[str, dict[str, set[str]]], dict[str, dict[str, Any]]]:
    packages = {p["id"]: p for p in metadata["packages"]}
    workspace = {package_id: packages[package_id] for package_id in metadata["workspace_members"]}
    by_name = {p["name"]: package_id for package_id, p in workspace.items()}
    graph = {kind: defaultdict(set) for kind in KINDS}
    for package_id, package in workspace.items():
        for dep in package["dependencies"]:
            target = by_name.get(dep.get("package") or dep["name"])
            if target is None:
                continue
            kind = dep.get("kind") or "normal"
            if kind in graph:
                graph[kind][package_id].add(target)
    return graph, workspace


def reachable(graph: dict[str, dict[str, set[str]]], roots: set[str], allowed: set[str]) -> set[str]:
    seen = set(roots)
    queue = deque(roots)
    while queue:
        current = queue.popleft()
        for kind in allowed:
            for target in graph[kind].get(current, ()):
                if target not in seen:
                    seen.add(target)
                    queue.append(target)
    return seen


def module_base(path: pathlib.Path) -> pathlib.Path:
    if path.name in {"lib.rs", "main.rs", "mod.rs"}:
        return path.parent
    return path.parent / path.stem


def active_modules(crate_root: pathlib.Path, package: dict[str, Any]) -> set[pathlib.Path]:
    active: set[pathlib.Path] = set()
    roots = {
        pathlib.Path(target["src_path"]).resolve()
        for target in package.get("targets", [])
        if target.get("src_path")
    }
    queue = deque(path for path in roots if path.exists())
    while queue:
        path = queue.popleft().resolve()
        if path in active or not path.is_file():
            continue
        active.add(path)
        text = path.read_text(encoding="utf-8")
        for relative in PATH_MOD_RE.findall(text):
            child = (path.parent / relative).resolve()
            if child.exists():
                queue.append(child)
        base = path.parent if path in roots else module_base(path)
        for name in MOD_RE.findall(text):
            candidates = (base / f"{name}.rs", base / name / "mod.rs")
            child = next((candidate for candidate in candidates if candidate.exists()), None)
            if child is not None:
                queue.append(child)
        for relative in INCLUDE_RE.findall(text):
            child = (path.parent / relative).resolve()
            try:
                child.relative_to(crate_root.resolve())
            except ValueError:
                continue
            if child.exists():
                queue.append(child)
        for relative in MANIFEST_INCLUDE_RE.findall(text):
            child = (crate_root / relative).resolve()
            if child.exists():
                queue.append(child)

    # Build-generated crate roots are production code too. Follow the source
    # files consumed by build.rs/build support so generated modules cannot hide
    # behind a blanket exemption.
    manifest = crate_root / "Cargo.toml"
    build_candidates = [crate_root / "build.rs"]
    if manifest.exists():
        manifest_text = manifest.read_text(encoding="utf-8")
        match = re.search(r'^build\s*=\s*"([^"]+)"', manifest_text, re.M)
        if match:
            build_candidates.append(crate_root / match.group(1))
    build_queue = deque(path.resolve() for path in build_candidates if path.exists())
    seen_build: set[pathlib.Path] = set()
    while build_queue:
        build_path = build_queue.popleft()
        if build_path in seen_build:
            continue
        seen_build.add(build_path)
        build_text = build_path.read_text(encoding="utf-8")
        for relative in INCLUDE_RE.findall(build_text):
            child = (build_path.parent / relative).resolve()
            if child.exists():
                build_queue.append(child)
        for relative in BUILD_SOURCE_RE.findall(build_text):
            child = (crate_root / relative).resolve()
            if child.exists():
                queue.append(child)
        for relative in RERUN_SOURCE_RE.findall(build_text):
            child = (crate_root / relative).resolve()
            if child.exists():
                queue.append(child)
        for relative in BOUND_MODULE_RE.findall(build_text):
            child = (crate_root / "src" / relative).resolve()
            if child.exists():
                queue.append(child)

    # Process any modules discovered through build generation.
    while queue:
        path = queue.popleft().resolve()
        if path in active or not path.is_file():
            continue
        active.add(path)
        text = path.read_text(encoding="utf-8")
        for relative in MANIFEST_INCLUDE_RE.findall(text):
            child = (crate_root / relative).resolve()
            if child.exists():
                queue.append(child)
        base = path.parent if path in roots else module_base(path)
        for name in MOD_RE.findall(text):
            child = next((candidate for candidate in (base / f"{name}.rs", base / name / "mod.rs") if candidate.exists()), None)
            if child is not None:
                queue.append(child)
    return active


def scan_hidden_dependencies(root: pathlib.Path, workspace: dict[str, dict[str, Any]], ignored_rs: set[str]) -> list[str]:
    errors: list[str] = []
    for path in root.glob("crates/*/**/*.rs"):
        rel = path.relative_to(root).as_posix()
        text = path.read_text(encoding="utf-8")
        if any(pattern.search(text) for pattern in SPLICE_PATTERNS):
            errors.append(f"hidden cross-crate source dependency: {rel}")
    for package in workspace.values():
        crate = pathlib.Path(package["manifest_path"]).resolve().parent
        src = crate / "src"
        if not src.is_dir():
            continue
        active = active_modules(crate, package)
        for path in src.rglob("*.rs"):
            rel = path.relative_to(root).as_posix()
            if path.resolve() not in active and rel not in ignored_rs:
                errors.append(f"unreferenced Rust source file: {rel}")
    return errors


def check(root: pathlib.Path, policy_path: pathlib.Path, report_path: pathlib.Path) -> int:
    policy = json.loads(policy_path.read_text(encoding="utf-8"))
    metadata = load_metadata(root)
    graph, workspace = dependency_graph(metadata)
    ids_by_name = {p["name"]: package_id for package_id, p in workspace.items()}
    errors: list[str] = []

    missing_roots = sorted(set(policy["shipped_roots"]) - set(ids_by_name))
    errors.extend(f"missing shipped root: {name}" for name in missing_roots)
    roots = {ids_by_name[name] for name in policy["shipped_roots"] if name in ids_by_name}
    production = reachable(graph, roots, {"normal", "build"})
    dev = reachable(graph, roots, {"normal", "build", "dev"})

    today = dt.date.today()
    exemptions = policy.get("crate_exemptions", {})
    for name, entry in exemptions.items():
        expiry = dt.date.fromisoformat(entry["expires"])
        if expiry < today:
            errors.append(f"expired crate exemption: {name} ({entry['expires']})")
        if not entry.get("owner") or not entry.get("reason"):
            errors.append(f"incomplete crate exemption: {name}")

    rows: list[tuple[str, str]] = []
    for package_id, package in sorted(workspace.items(), key=lambda item: item[1]["name"]):
        name = package["name"]
        if package_id in production:
            status = "production"
        elif package_id in dev and name in policy.get("test_support_crates", []):
            status = "dev-only test support"
        elif name in exemptions:
            status = f"exempt until {exemptions[name]['expires']}"
        else:
            status = "UNREACHABLE"
            errors.append(f"workspace crate unreachable from shipped roots: {name}")
        rows.append((name, status))

    expected_bins = set(policy.get("expected_binary_targets", []))
    actual_bins = {
        target["name"]
        for package in workspace.values()
        for target in package.get("targets", [])
        if "bin" in target.get("kind", [])
    }
    for name in sorted(expected_bins - actual_bins):
        errors.append(f"expected binary target missing: {name}")

    errors.extend(scan_hidden_dependencies(root, workspace, set(policy.get("ignored_unreferenced_rs", []))))
    lines = ["# Medusa architecture policy report", "", "| Crate | Reachability |", "|---|---|"]
    lines.extend(f"| `{name}` | {status} |" for name, status in rows)
    lines.extend(["", "## Violations", ""])
    lines.extend(f"- {error}" for error in errors) if errors else lines.append("None.")
    report_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(report_path.read_text(encoding="utf-8"))
    return 1 if errors else 0


def self_test() -> int:
    expired = {"owner": "architecture", "reason": "fixture", "expires": "2000-01-01"}
    assert dt.date.fromisoformat(expired["expires"]) < dt.date.today()
    assert any(pattern.search('include!("../../crates/x/src/lib.rs")') for pattern in SPLICE_PATTERNS)
    graph = {kind: defaultdict(set) for kind in KINDS}
    graph["normal"]["root"].add("normal")
    graph["build"]["root"].add("build")
    graph["dev"]["root"].add("dev")
    assert reachable(graph, {"root"}, {"normal", "build"}) == {"root", "normal", "build"}
    assert "dev" in reachable(graph, {"root"}, set(KINDS))
    assert "orphan" not in reachable(graph, {"root"}, set(KINDS))
    assert module_base(pathlib.Path("src/server.rs")) == pathlib.Path("src/server")
    assert module_base(pathlib.Path("src/lib.rs")) == pathlib.Path("src")
    assert MANIFEST_INCLUDE_RE.findall('include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine.rs"))') == ["src/engine.rs"]
    assert BUILD_SOURCE_RE.findall('fs::read_to_string("src/runtime_impl.rs")?') == ["src/runtime_impl.rs"]
    assert RERUN_SOURCE_RE.findall('println!("cargo:rerun-if-changed=src/runtime_impl.rs");') == ["src/runtime_impl.rs"]
    assert BOUND_MODULE_RE.findall('("mod error;", "error.rs")') == ["error.rs"]
    print("architecture policy self-test passed")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)
    check_parser = sub.add_parser("check")
    check_parser.add_argument("--root", type=pathlib.Path, required=True)
    check_parser.add_argument("--policy", type=pathlib.Path, required=True)
    check_parser.add_argument("--report", type=pathlib.Path, required=True)
    sub.add_parser("self-test")
    args = parser.parse_args()
    if args.command == "self-test":
        return self_test()
    return check(args.root.resolve(), args.policy, args.report)


if __name__ == "__main__":
    sys.exit(main())
