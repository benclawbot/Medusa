#!/usr/bin/env python3
"""Package the native Medusa CLI for the rolling main-branch updater."""

from __future__ import annotations

import argparse
import hashlib
import json
import platform
import re
import tarfile
import zipfile
from pathlib import Path

SCHEMA = "medusa-main-artifact-v1"
REVISION = re.compile(r"^[0-9a-f]{40}$")


def platform_name() -> str:
    system = platform.system()
    try:
        return {"Linux": "linux", "Darwin": "macos", "Windows": "windows"}[system]
    except KeyError as exc:
        raise SystemExit(f"unsupported rolling-update platform: {system}") from exc


def architecture_name() -> str:
    machine = platform.machine().lower()
    if machine in {"x86_64", "amd64"}:
        return "x86_64"
    if machine in {"aarch64", "arm64"}:
        return "aarch64"
    raise SystemExit(f"unsupported rolling-update architecture: {machine}")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def package(revision: str, binary: Path, output: Path) -> tuple[Path, Path]:
    if not REVISION.fullmatch(revision):
        raise SystemExit("revision must be a full lowercase 40-character Git SHA")
    if not binary.is_file() or binary.stat().st_size == 0:
        raise SystemExit(f"Medusa binary is missing or empty: {binary}")

    system = platform_name()
    architecture = architecture_name()
    output.mkdir(parents=True, exist_ok=True)

    if system == "windows":
        archive = output / f"medusa-main-{system}-{architecture}.zip"
        with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as bundle:
            bundle.write(binary, arcname="medusa.exe")
    else:
        archive = output / f"medusa-main-{system}-{architecture}.tar.gz"
        with tarfile.open(archive, "w:gz") as bundle:
            bundle.add(binary, arcname="medusa")

    manifest = output / f"{archive.name}.json"
    manifest.write_text(
        json.dumps(
            {
                "schema": SCHEMA,
                "revision": revision,
                "name": archive.name,
                "bytes": archive.stat().st_size,
                "sha256": sha256(archive),
            },
            sort_keys=True,
            separators=(",", ":"),
        )
        + "\n",
        encoding="utf-8",
    )
    return archive, manifest


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--revision", required=True)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    archive, manifest = package(args.revision, args.binary, args.output)
    print(archive)
    print(manifest)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
