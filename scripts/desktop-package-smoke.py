#!/usr/bin/env python3
"""Validate unsigned Medusa Desktop bundles and emit SHA-256 evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import tempfile
from pathlib import Path

MIN_ARTIFACT_BYTES = 1024 * 1024

EXPECTED = {
    "linux": (
        ("deb", "deb", "*.deb", "file"),
        ("appimage", "appimage", "*.AppImage", "file"),
    ),
    "macos": (
        ("app", "macos", "*.app", "directory"),
        ("dmg", "dmg", "*.dmg", "file"),
    ),
    "windows": (("nsis", "nsis", "*.exe", "file"),),
}


def within(path: Path, root: Path) -> bool:
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def validate_symlink(path: Path, allowed_root: Path) -> None:
    if not path.is_symlink():
        return
    target = path.resolve(strict=True)
    if not within(target, allowed_root):
        raise ValueError(f"symlink escapes bundle root: {path} -> {target}")


def file_digest(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def directory_evidence(path: Path, allowed_root: Path) -> tuple[int, str]:
    digest = hashlib.sha256()
    total = 0
    for current, directories, files in os.walk(path, followlinks=False):
        current_path = Path(current)
        directories.sort()
        files.sort()
        for name in directories + files:
            entry = current_path / name
            validate_symlink(entry, allowed_root)
            relative = entry.relative_to(path).as_posix().encode("utf-8")
            digest.update(relative)
            digest.update(b"\0")
            if entry.is_symlink():
                digest.update(os.readlink(entry).encode("utf-8"))
                digest.update(b"\0")
            elif entry.is_file():
                size = entry.stat().st_size
                total += size
                digest.update(str(size).encode("ascii"))
                digest.update(b"\0")
                with entry.open("rb") as source:
                    for chunk in iter(lambda: source.read(1024 * 1024), b""):
                        digest.update(chunk)
    return total, digest.hexdigest()


def find_one(root: Path, directory: str, pattern: str) -> Path:
    container = (root / directory).resolve()
    if not within(container, root):
        raise ValueError(f"bundle directory escapes root: {container}")
    matches = sorted(container.glob(pattern)) if container.is_dir() else []
    if len(matches) != 1:
        raise ValueError(
            f"expected exactly one {directory}/{pattern} artifact, found {len(matches)}"
        )
    artifact = matches[0]
    if artifact.is_symlink():
        validate_symlink(artifact, root)
    resolved = artifact.resolve(strict=True)
    if not within(resolved, root):
        raise ValueError(f"artifact escapes bundle root: {artifact} -> {resolved}")
    return artifact


def inspect(root: Path, platform: str) -> dict[str, object]:
    root = root.resolve(strict=True)
    if platform not in EXPECTED:
        raise ValueError(f"unsupported platform {platform!r}")
    artifacts: list[dict[str, object]] = []
    for kind, directory, pattern, expected_type in EXPECTED[platform]:
        artifact = find_one(root, directory, pattern)
        if expected_type == "file":
            if not artifact.is_file():
                raise ValueError(f"{kind} artifact is not a file: {artifact}")
            size = artifact.stat().st_size
            digest = file_digest(artifact)
        else:
            if not artifact.is_dir():
                raise ValueError(f"{kind} artifact is not a directory: {artifact}")
            size, digest = directory_evidence(artifact, root)
        if size < MIN_ARTIFACT_BYTES:
            raise ValueError(
                f"{kind} artifact is unexpectedly small: {size} bytes "
                f"(minimum {MIN_ARTIFACT_BYTES})"
            )
        artifacts.append(
            {
                "kind": kind,
                "path": artifact.relative_to(root).as_posix(),
                "bytes": size,
                "sha256": digest,
            }
        )
    return {"platform": platform, "bundle_root": str(root), "artifacts": artifacts}


def create_large_file(path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("wb") as output:
        output.truncate(MIN_ARTIFACT_BYTES + 1)


def create_fixture(root: Path, platform: str) -> None:
    if platform == "linux":
        create_large_file(root / "deb/medusa-desktop.deb")
        create_large_file(root / "appimage/Medusa_Desktop.AppImage")
    elif platform == "macos":
        create_large_file(root / "macos/Medusa Desktop.app/Contents/MacOS/medusa-desktop")
        create_large_file(root / "dmg/Medusa Desktop.dmg")
    elif platform == "windows":
        create_large_file(root / "nsis/Medusa Desktop_1.0.0_x64-setup.exe")
    else:
        raise AssertionError(platform)


def expect_failure(root: Path, platform: str, fragment: str) -> None:
    try:
        inspect(root, platform)
    except (OSError, ValueError) as error:
        if fragment not in str(error):
            raise AssertionError(f"expected {fragment!r} in {error!r}") from error
    else:
        raise AssertionError(f"{platform} fixture unexpectedly passed")


def self_test() -> None:
    for platform in EXPECTED:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            create_fixture(root, platform)
            evidence = inspect(root, platform)
            assert evidence["platform"] == platform
            assert len(evidence["artifacts"]) == len(EXPECTED[platform])
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        create_fixture(root, "windows")
        (root / "nsis/Medusa Desktop_1.0.0_x64-setup.exe").unlink()
        expect_failure(root, "windows", "expected exactly one")
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        create_fixture(root, "linux")
        tiny = root / "deb/medusa-desktop.deb"
        tiny.write_bytes(b"tiny")
        expect_failure(root, "linux", "unexpectedly small")
    if hasattr(os, "symlink"):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            outside = root.parent / f"{root.name}-outside.exe"
            create_large_file(outside)
            (root / "nsis").mkdir()
            try:
                os.symlink(outside, root / "nsis/Medusa Desktop_1.0.0_x64-setup.exe")
            except OSError:
                outside.unlink(missing_ok=True)
            else:
                try:
                    expect_failure(root, "windows", "escapes bundle root")
                finally:
                    outside.unlink(missing_ok=True)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path)
    parser.add_argument("--platform", choices=sorted(EXPECTED))
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
    if args.root is None and args.platform is None and args.manifest is None:
        return 0
    if args.root is None or args.platform is None or args.manifest is None:
        parser.error("--root, --platform, and --manifest must be supplied together")
    try:
        evidence = inspect(args.root, args.platform)
        args.manifest.parent.mkdir(parents=True, exist_ok=True)
        args.manifest.write_text(
            json.dumps(evidence, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
    except (OSError, ValueError) as error:
        print(f"desktop package smoke failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(evidence, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
