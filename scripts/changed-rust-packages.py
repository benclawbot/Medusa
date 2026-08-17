#!/usr/bin/env python3
"""Select workspace packages whose source trees changed in a pull request."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parents[1]
GLOBAL_RUST_INPUTS = {
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain",
    "rust-toolchain.toml",
}
SAFE_PACKAGE = re.compile(r"^[A-Za-z0-9_.-]+$")


def run(*args: str) -> str:
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def load_metadata(path: Path | None) -> dict:
    if path is not None:
        return json.loads(path.read_text(encoding="utf-8"))
    return json.loads(run("cargo", "metadata", "--locked", "--format-version", "1", "--no-deps"))


def load_changed_files(base: str | None, path: Path | None) -> list[str]:
    if path is not None:
        return [line.strip() for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]
    if not base:
        raise SystemExit("--base is required unless --changed-files is supplied")
    output = run("git", "diff", "--name-only", "--diff-filter=ACMR", base, "HEAD")
    return [line for line in output.splitlines() if line]


def manifest_directory(manifest_path: str) -> PurePosixPath:
    manifest = Path(manifest_path)
    if not manifest.is_absolute():
        manifest = ROOT / manifest
    try:
        relative = manifest.resolve().parent.relative_to(ROOT.resolve())
    except ValueError as exc:
        raise SystemExit(f"workspace manifest outside repository: {manifest}") from exc
    return PurePosixPath(relative.as_posix() or ".")


def package_directories(metadata: dict) -> list[tuple[str, PurePosixPath]]:
    packages: list[tuple[str, PurePosixPath]] = []
    for package in metadata.get("packages", []):
        name = package["name"]
        if not SAFE_PACKAGE.fullmatch(name):
            raise SystemExit(f"unsafe Cargo package name: {name!r}")
        packages.append((name, manifest_directory(package["manifest_path"])))
    return packages


def path_is_global(path: str) -> bool:
    normalized = PurePosixPath(path).as_posix()
    return normalized in GLOBAL_RUST_INPUTS or normalized.startswith(".cargo/")


def containing_packages(
    changed_path: str, packages: list[tuple[str, PurePosixPath]]
) -> set[str]:
    path = PurePosixPath(changed_path)
    matches: list[tuple[str, PurePosixPath]] = []
    for name, directory in packages:
        if directory == PurePosixPath("."):
            if path.parts and path.parts[0] in {"src", "tests", "examples", "benches"}:
                matches.append((name, directory))
            elif path.as_posix() == "build.rs":
                matches.append((name, directory))
            continue
        if path == directory or directory in path.parents:
            matches.append((name, directory))
    if not matches:
        return set()
    deepest = max(len(directory.parts) for _, directory in matches)
    return {name for name, directory in matches if len(directory.parts) == deepest}


def select_packages(metadata: dict, changed_files: list[str]) -> list[str]:
    packages = package_directories(metadata)
    if any(path_is_global(path) for path in changed_files):
        return sorted(name for name, _ in packages)

    selected: set[str] = set()
    for path in changed_files:
        selected.update(containing_packages(path, packages))
    return sorted(selected)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base")
    parser.add_argument("--metadata-json", type=Path)
    parser.add_argument("--changed-files", type=Path)
    parser.add_argument("--format", choices=("names", "cargo-args"), default="names")
    args = parser.parse_args()

    selected = select_packages(
        load_metadata(args.metadata_json),
        load_changed_files(args.base, args.changed_files),
    )
    if args.format == "cargo-args":
        print(" ".join(f"-p {name}" for name in selected))
    else:
        print("\n".join(selected))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
