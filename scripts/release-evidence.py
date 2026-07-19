#!/usr/bin/env python3
"""Generate deterministic Medusa release evidence without third-party dependencies."""

from __future__ import annotations

import argparse
import fnmatch
import hashlib
import json
import os
from pathlib import Path
import tempfile
import tomllib
import urllib.parse
import uuid

REQUIRED_ASSETS = {
    "linux CLI archive": "medusa-cli-linux.tar.gz",
    "macOS CLI archive": "medusa-cli-macos.tar.gz",
    "Windows CLI archive": "medusa-cli-windows.zip",
    "Linux Debian package": "medusa-desktop-linux.deb",
    "Linux AppImage": "medusa-desktop-linux.AppImage",
    "macOS application archive": "medusa-desktop-macos-app.zip",
    "macOS disk image": "medusa-desktop-macos.dmg",
    "Windows NSIS installer": "medusa-desktop-windows.exe",
    "CycloneDX SBOM": "medusa-sbom.cdx.json",
    "license": "LICENSE",
    "release guide": "RELEASE.md",
    "compatibility notes": "COMPATIBILITY.md",
}


class EvidenceError(RuntimeError):
    """Raised when release evidence is incomplete or unsafe."""


def read_toml(path: Path) -> dict:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def load_versions(root: Path) -> dict[str, str]:
    versions = {
        "workspace": str(read_toml(root / "Cargo.toml")["workspace"]["package"]["version"]),
        "desktop-cargo": str(
            read_toml(root / "apps/medusa-desktop/src-tauri/Cargo.toml")["package"]["version"]
        ),
        "desktop-npm": str(
            json.loads((root / "apps/medusa-desktop/package.json").read_text(encoding="utf-8"))[
                "version"
            ]
        ),
        "desktop-tauri": str(
            json.loads(
                (root / "apps/medusa-desktop/src-tauri/tauri.conf.json").read_text(
                    encoding="utf-8"
                )
            )["version"]
        ),
    }
    if len(set(versions.values())) != 1:
        rendered = ", ".join(f"{name}={value}" for name, value in sorted(versions.items()))
        raise EvidenceError(f"release version metadata is not synchronized: {rendered}")
    return versions


def validate_tag(root: Path, tag: str) -> str:
    versions = load_versions(root)
    version = next(iter(versions.values()))
    expected = f"v{version}"
    if tag != expected:
        raise EvidenceError(f"release tag must be {expected}, got {tag}")
    return version


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def cargo_components(root: Path) -> list[dict]:
    lock = read_toml(root / "Cargo.lock")
    components: list[dict] = []
    for package in lock.get("package", []):
        name = str(package["name"])
        version = str(package["version"])
        source = str(package.get("source", "workspace"))
        purl = f"pkg:cargo/{urllib.parse.quote(name, safe='')}@{version}"
        source_key = hashlib.sha256(source.encode("utf-8")).hexdigest()[:12]
        component = {
            "type": "library",
            "bom-ref": f"{purl}?source={source_key}",
            "name": name,
            "version": version,
            "purl": purl,
            "properties": [
                {"name": "medusa:ecosystem", "value": "cargo"},
                {"name": "medusa:source", "value": source},
            ],
        }
        checksum = package.get("checksum")
        if checksum:
            component["hashes"] = [{"alg": "SHA-256", "content": str(checksum)}]
        components.append(component)
    return components


def npm_components(root: Path) -> list[dict]:
    lock_path = root / "apps/medusa-desktop/package-lock.json"
    lock = json.loads(lock_path.read_text(encoding="utf-8"))
    components: list[dict] = []
    for package_path, package in sorted(lock.get("packages", {}).items()):
        if not package_path or not package.get("version"):
            continue
        name = str(package.get("name") or Path(package_path).name)
        version = str(package["version"])
        purl = f"pkg:npm/{urllib.parse.quote(name, safe='')}@{version}"
        component = {
            "type": "library",
            "bom-ref": f"{purl}?path={urllib.parse.quote(package_path, safe='')}",
            "name": name,
            "version": version,
            "purl": purl,
            "properties": [
                {"name": "medusa:ecosystem", "value": "npm"},
                {"name": "medusa:lock-path", "value": package_path},
            ],
        }
        license_value = package.get("license")
        if isinstance(license_value, str) and license_value.strip():
            component["licenses"] = [{"license": {"name": license_value.strip()}}]
        integrity = package.get("integrity")
        if isinstance(integrity, str) and integrity.startswith("sha512-"):
            component["properties"].append(
                {"name": "medusa:npm-integrity", "value": integrity}
            )
        components.append(component)
    return components


def generate_sbom(root: Path, output: Path) -> dict:
    version = next(iter(load_versions(root).values()))
    components = cargo_components(root) + npm_components(root)
    components.sort(
        key=lambda item: (
            item["properties"][0]["value"],
            item["name"],
            item["version"],
            item["bom-ref"],
        )
    )
    component_digest = hashlib.sha256(
        json.dumps(components, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ).digest()
    serial = uuid.UUID(bytes=component_digest[:16], version=5)
    sbom = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.6",
        "serialNumber": f"urn:uuid:{serial}",
        "version": 1,
        "metadata": {
            "component": {
                "type": "application",
                "name": "medusa",
                "version": version,
                "purl": f"pkg:github/benclawbot/Medusa@v{version}",
            },
            "properties": [
                {"name": "medusa:source-locks", "value": "Cargo.lock,package-lock.json"}
            ],
        },
        "components": components,
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(sbom, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return sbom


def safe_files(root: Path, excluded: set[Path]) -> list[Path]:
    resolved_root = root.resolve(strict=True)
    files: list[Path] = []
    seen_names: set[str] = set()
    for candidate in sorted(root.rglob("*")):
        if candidate in excluded:
            continue
        if candidate.is_symlink():
            raise EvidenceError(f"release assets cannot contain symlinks: {candidate}")
        if not candidate.is_file():
            continue
        resolved = candidate.resolve(strict=True)
        if resolved_root not in resolved.parents:
            raise EvidenceError(f"release asset escapes root: {candidate}")
        relative = candidate.relative_to(root)
        if relative.name in seen_names:
            raise EvidenceError(f"duplicate release asset basename: {relative.name}")
        seen_names.add(relative.name)
        files.append(candidate)
    return files


def validate_required_assets(files: list[Path]) -> None:
    names = [path.name for path in files]
    for label, pattern in REQUIRED_ASSETS.items():
        matches = [name for name in names if fnmatch.fnmatchcase(name, pattern)]
        if len(matches) != 1:
            raise EvidenceError(
                f"expected exactly one {label} matching {pattern}, found {len(matches)}"
            )


def generate_manifest(assets: Path, output: Path, checksums: Path) -> dict:
    assets.mkdir(parents=True, exist_ok=True)
    excluded = {output, checksums}
    files = safe_files(assets, excluded)
    validate_required_assets(files)
    entries = [
        {
            "path": path.relative_to(assets).as_posix(),
            "bytes": path.stat().st_size,
            "sha256": sha256_file(path),
        }
        for path in files
    ]
    manifest = {
        "schema": "medusa-release-manifest-v1",
        "assets": entries,
    }
    output.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    checksum_lines = [f"{entry['sha256']}  {entry['path']}" for entry in entries]
    checksums.write_text("\n".join(checksum_lines) + "\n", encoding="utf-8")
    return manifest


def write_minimal_fixture(root: Path) -> None:
    (root / "apps/medusa-desktop/src-tauri").mkdir(parents=True)
    (root / "apps/medusa-desktop").mkdir(parents=True, exist_ok=True)
    (root / "Cargo.toml").write_text(
        '[workspace]\n[workspace.package]\nversion = "1.2.3"\n', encoding="utf-8"
    )
    (root / "Cargo.lock").write_text(
        'version = 4\n[[package]]\nname = "fixture"\nversion = "1.0.0"\n',
        encoding="utf-8",
    )
    (root / "apps/medusa-desktop/src-tauri/Cargo.toml").write_text(
        '[package]\nname = "desktop"\nversion = "1.2.3"\n', encoding="utf-8"
    )
    (root / "apps/medusa-desktop/package.json").write_text(
        json.dumps({"name": "desktop", "version": "1.2.3"}), encoding="utf-8"
    )
    (root / "apps/medusa-desktop/src-tauri/tauri.conf.json").write_text(
        json.dumps({"version": "1.2.3"}), encoding="utf-8"
    )
    (root / "apps/medusa-desktop/package-lock.json").write_text(
        json.dumps(
            {
                "lockfileVersion": 3,
                "packages": {
                    "": {"name": "desktop", "version": "1.2.3"},
                    "node_modules/example": {
                        "name": "example",
                        "version": "2.0.0",
                        "license": "MIT",
                    },
                },
            }
        ),
        encoding="utf-8",
    )


def populate_assets(assets: Path) -> None:
    assets.mkdir(parents=True)
    for name in REQUIRED_ASSETS.values():
        (assets / name).write_bytes(f"fixture:{name}\n".encode("utf-8"))


def self_test() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        write_minimal_fixture(root)
        assert validate_tag(root, "v1.2.3") == "1.2.3"
        try:
            validate_tag(root, "v1.2.4")
        except EvidenceError:
            pass
        else:
            raise AssertionError("mismatched release tag was accepted")

        first = root / "first-sbom.json"
        second = root / "second-sbom.json"
        generate_sbom(root, first)
        generate_sbom(root, second)
        assert first.read_bytes() == second.read_bytes()

        assets = root / "assets"
        populate_assets(assets)
        manifest = assets / "release-manifest.json"
        checksums = assets / "SHA256SUMS"
        first_manifest = generate_manifest(assets, manifest, checksums)
        second_manifest = generate_manifest(assets, manifest, checksums)
        assert first_manifest == second_manifest

        duplicate = assets / "nested"
        duplicate.mkdir()
        (duplicate / "LICENSE").write_text("duplicate", encoding="utf-8")
        try:
            generate_manifest(assets, manifest, checksums)
        except EvidenceError:
            pass
        else:
            raise AssertionError("duplicate asset basename was accepted")
        (duplicate / "LICENSE").unlink()

        if hasattr(os, "symlink"):
            outside = root / "outside"
            outside.write_text("outside", encoding="utf-8")
            link = assets / "escape-link"
            try:
                link.symlink_to(outside)
            except OSError:
                pass
            else:
                try:
                    generate_manifest(assets, manifest, checksums)
                except EvidenceError:
                    pass
                else:
                    raise AssertionError("symlink release asset was accepted")

    print("release-evidence-self-test-ok")


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    subcommands = result.add_subparsers(dest="command", required=True)

    validate = subcommands.add_parser("validate-tag")
    validate.add_argument("--root", type=Path, default=Path("."))
    validate.add_argument("--tag", required=True)

    sbom = subcommands.add_parser("sbom")
    sbom.add_argument("--root", type=Path, default=Path("."))
    sbom.add_argument("--output", type=Path, required=True)

    manifest = subcommands.add_parser("manifest")
    manifest.add_argument("--assets", type=Path, required=True)
    manifest.add_argument("--output", type=Path, required=True)
    manifest.add_argument("--checksums", type=Path, required=True)

    subcommands.add_parser("self-test")
    return result


def main() -> int:
    arguments = parser().parse_args()
    try:
        if arguments.command == "validate-tag":
            version = validate_tag(arguments.root, arguments.tag)
            print(version)
        elif arguments.command == "sbom":
            generate_sbom(arguments.root, arguments.output)
            print(arguments.output)
        elif arguments.command == "manifest":
            generate_manifest(arguments.assets, arguments.output, arguments.checksums)
            print(arguments.output)
        else:
            self_test()
    except (EvidenceError, KeyError, OSError, ValueError, tomllib.TOMLDecodeError) as error:
        print(f"release evidence error: {error}", file=os.sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
