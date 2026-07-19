#!/usr/bin/env python3
"""Validate that Medusa desktop release versions stay synchronized."""

from __future__ import annotations

import argparse
import json
import re
import sys
import tempfile
import tomllib
from pathlib import Path
from typing import Mapping

SEMVER = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?$")


def load_versions(root: Path) -> dict[str, str]:
    workspace = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    desktop_cargo = tomllib.loads(
        (root / "apps/medusa-desktop/src-tauri/Cargo.toml").read_text(encoding="utf-8")
    )
    package = json.loads(
        (root / "apps/medusa-desktop/package.json").read_text(encoding="utf-8")
    )
    tauri = json.loads(
        (root / "apps/medusa-desktop/src-tauri/tauri.conf.json").read_text(
            encoding="utf-8"
        )
    )
    return {
        "workspace": str(workspace["workspace"]["package"]["version"]),
        "desktop_cargo": str(desktop_cargo["package"]["version"]),
        "package_json": str(package["version"]),
        "tauri_config": str(tauri["version"]),
    }


def validate_versions(versions: Mapping[str, str]) -> str:
    invalid = {name: value for name, value in versions.items() if not SEMVER.fullmatch(value)}
    if invalid:
        details = ", ".join(f"{name}={value!r}" for name, value in invalid.items())
        raise ValueError(f"invalid semantic version values: {details}")
    unique = sorted(set(versions.values()))
    if len(unique) != 1:
        details = ", ".join(f"{name}={value}" for name, value in versions.items())
        raise ValueError(f"desktop release versions are not synchronized: {details}")
    return unique[0]


def write_fixture(root: Path, versions: Mapping[str, str]) -> None:
    (root / "apps/medusa-desktop/src-tauri").mkdir(parents=True, exist_ok=True)
    (root / "Cargo.toml").write_text(
        f'[workspace]\n[workspace.package]\nversion = "{versions["workspace"]}"\n',
        encoding="utf-8",
    )
    (root / "apps/medusa-desktop/src-tauri/Cargo.toml").write_text(
        f'[package]\nname = "medusa-desktop"\nversion = "{versions["desktop_cargo"]}"\n',
        encoding="utf-8",
    )
    (root / "apps/medusa-desktop/package.json").write_text(
        json.dumps({"version": versions["package_json"]}),
        encoding="utf-8",
    )
    (root / "apps/medusa-desktop/src-tauri/tauri.conf.json").write_text(
        json.dumps({"version": versions["tauri_config"]}),
        encoding="utf-8",
    )


def self_test() -> None:
    matching = {
        "workspace": "1.2.3",
        "desktop_cargo": "1.2.3",
        "package_json": "1.2.3",
        "tauri_config": "1.2.3",
    }
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        write_fixture(root, matching)
        assert validate_versions(load_versions(root)) == "1.2.3"
        mismatched = dict(matching)
        mismatched["package_json"] = "1.2.4"
        write_fixture(root, mismatched)
        try:
            validate_versions(load_versions(root))
        except ValueError as error:
            assert "not synchronized" in str(error)
        else:
            raise AssertionError("mismatched versions were accepted")
        invalid = dict(matching)
        invalid["tauri_config"] = "latest"
        write_fixture(root, invalid)
        try:
            validate_versions(load_versions(root))
        except ValueError as error:
            assert "invalid semantic version" in str(error)
        else:
            raise AssertionError("invalid version was accepted")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
    try:
        versions = load_versions(args.root.resolve())
        version = validate_versions(versions)
    except (
        OSError,
        KeyError,
        TypeError,
        ValueError,
        tomllib.TOMLDecodeError,
        json.JSONDecodeError,
    ) as error:
        print(f"desktop version check failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps({"version": version, "sources": versions}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
