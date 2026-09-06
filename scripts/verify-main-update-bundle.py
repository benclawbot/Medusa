#!/usr/bin/env python3
"""Verify that every platform artifact in a rolling update bundle is exact."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path

SCHEMA = "medusa-main-artifact-v1"
SIGNATURE_SCHEMA = "medusa-release-signature-v1"
SIGNATURE_KEY_ID = "medusa-release-2026-08-primary"
SIGNATURE_ALGORITHM = "Ed25519"
REVISION = re.compile(r"^[0-9a-f]{40}$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")
SIGNATURE_HEX = re.compile(r"^[0-9a-f]{128}$")
EXPECTED_ARCHIVES = {
    "linux-x86_64": "medusa-main-linux-x86_64.tar.gz",
    "macos-aarch64": "medusa-main-macos-aarch64.tar.gz",
    "windows-x86_64": "medusa-main-windows-x86_64.zip",
}
EXPECTED_DESKTOP = {
    "linux-x86_64": "medusa-desktop-main-linux-x86_64",
    "macos-aarch64": "medusa-desktop-main-macos-aarch64",
    "windows-x86_64": "medusa-desktop-main-windows-x86_64.exe",
}
EXPECTED_ARTIFACTS = {**EXPECTED_ARCHIVES, **{
    f"desktop-{platform}": name for platform, name in EXPECTED_DESKTOP.items()
}}
EXPECTED_KINDS = {
    "all": EXPECTED_ARTIFACTS,
    "cli": EXPECTED_ARCHIVES,
    "desktop": EXPECTED_DESKTOP,
}
MANIFEST_KEYS = {"bytes", "name", "revision", "schema", "sha256"}
SIGNATURE_KEYS = {"schema", "key_id", "algorithm", "manifest_sha256", "signature"}


def file_digest(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def verify_bundle(bundle: Path, revision: str, kind: str = "all") -> tuple[str, ...]:
    if not REVISION.fullmatch(revision):
        raise ValueError("revision must be a full lowercase 40-character Git SHA")
    try:
        expected = EXPECTED_KINDS[kind]
    except KeyError as exc:
        raise ValueError(f"unknown rolling bundle kind: {kind}") from exc
    root = bundle.resolve(strict=True)
    if not root.is_dir():
        raise ValueError(f"bundle path is not a directory: {bundle}")

    expected_artifacts = set(expected.values())
    expected_files = (
        expected_artifacts
        | {f"{name}.json" for name in expected_artifacts}
        | {f"{name}.json.sig.json" for name in expected_artifacts}
    )
    entries = {entry.name: entry for entry in root.iterdir()}
    if set(entries) != expected_files:
        missing = sorted(expected_files - set(entries))
        extra = sorted(set(entries) - expected_files)
        details = []
        if missing:
            details.append(f"missing {', '.join(missing)}")
        if extra:
            details.append(f"unexpected {', '.join(extra)}")
        raise ValueError("bundle does not contain exactly the expected files: " + "; ".join(details))

    for name in sorted(expected_artifacts):
        archive = entries[name]
        manifest_path = entries[f"{name}.json"]
        signature_path = entries[f"{name}.json.sig.json"]
        if archive.is_symlink() or not archive.is_file() or archive.stat().st_size == 0:
            raise ValueError(f"artifact is not a non-empty regular file: {name}")
        if manifest_path.is_symlink() or not manifest_path.is_file():
            raise ValueError(f"manifest is not a regular file: {manifest_path.name}")
        if signature_path.is_symlink() or not signature_path.is_file():
            raise ValueError(f"signature is not a regular file: {signature_path.name}")
        try:
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, json.JSONDecodeError) as error:
            raise ValueError(f"could not parse manifest {manifest_path.name}: {error}") from error
        if not isinstance(manifest, dict) or set(manifest) != MANIFEST_KEYS:
            raise ValueError(f"manifest has an unexpected schema: {manifest_path.name}")
        if manifest["schema"] != SCHEMA:
            raise ValueError(f"manifest schema mismatch: {manifest_path.name}")
        if manifest["revision"] != revision:
            raise ValueError(
                f"manifest revision mismatch for {name}: "
                f"{manifest['revision']} != {revision}"
            )
        if manifest["name"] != name:
            raise ValueError(f"manifest name mismatch: {manifest_path.name}")
        size = manifest["bytes"]
        if isinstance(size, bool) or not isinstance(size, int) or size <= 0:
            raise ValueError(f"manifest byte count is invalid: {manifest_path.name}")
        if size != archive.stat().st_size:
            raise ValueError(
                f"manifest byte count mismatch for {name}: {size} != {archive.stat().st_size}"
            )
        digest = manifest["sha256"]
        if not isinstance(digest, str) or not SHA256.fullmatch(digest):
            raise ValueError(f"manifest digest is invalid: {manifest_path.name}")
        actual_digest = file_digest(archive)
        if digest != actual_digest:
            raise ValueError(
                f"manifest digest mismatch for {name}: {digest} != {actual_digest}"
            )

        try:
            signature = json.loads(signature_path.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, json.JSONDecodeError) as error:
            raise ValueError(f"could not parse signature {signature_path.name}: {error}") from error
        if not isinstance(signature, dict) or set(signature) != SIGNATURE_KEYS:
            raise ValueError(f"signature has an unexpected schema: {signature_path.name}")
        if signature["schema"] != SIGNATURE_SCHEMA:
            raise ValueError(f"signature schema mismatch: {signature_path.name}")
        if signature["key_id"] != SIGNATURE_KEY_ID:
            raise ValueError(f"signature key id mismatch: {signature_path.name}")
        if signature["algorithm"] != SIGNATURE_ALGORITHM:
            raise ValueError(f"signature algorithm mismatch: {signature_path.name}")
        manifest_digest = signature["manifest_sha256"]
        if not isinstance(manifest_digest, str) or not SHA256.fullmatch(manifest_digest):
            raise ValueError(f"signature manifest digest is invalid: {signature_path.name}")
        actual_manifest_digest = file_digest(manifest_path)
        if manifest_digest != actual_manifest_digest:
            raise ValueError(
                f"signature manifest digest mismatch for {name}: "
                f"{manifest_digest} != {actual_manifest_digest}"
            )
        signature_hex = signature["signature"]
        if not isinstance(signature_hex, str) or not SIGNATURE_HEX.fullmatch(signature_hex):
            raise ValueError(f"signature bytes are invalid: {signature_path.name}")
    return tuple(sorted(expected_artifacts))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--bundle", type=Path, required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--kind", choices=tuple(EXPECTED_KINDS), default="all")
    args = parser.parse_args()
    try:
        names = verify_bundle(args.bundle, args.revision, args.kind)
    except (OSError, ValueError) as error:
        print(f"rolling main update bundle verification failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps({"revision": args.revision, "artifacts": names}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
