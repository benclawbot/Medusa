#!/usr/bin/env python3
"""Generate deterministic, signed Medusa release evidence without Python packages."""

from __future__ import annotations

import argparse
import fnmatch
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import tempfile
import tomllib
import urllib.parse
import uuid

MANIFEST_SCHEMA = "medusa-release-manifest-v2"
SIGNATURE_SCHEMA = "medusa-release-signature-v1"
SIGNATURE_NAME = "medusa-release-manifest.sig.json"
DEFAULT_KEY_ID = "medusa-release-2026-01"

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

INSTALLABLE_ASSETS = {
    "medusa-cli-linux.tar.gz": ("cli-archive", "linux", "x86_64", "x86_64-unknown-linux-gnu"),
    "medusa-cli-macos.tar.gz": ("cli-archive", "macos", "x86_64", "x86_64-apple-darwin"),
    "medusa-cli-windows.zip": ("cli-archive", "windows", "x86_64", "x86_64-pc-windows-msvc"),
    "medusa-desktop-linux.deb": ("desktop-package", "linux", "x86_64", "x86_64-unknown-linux-gnu"),
    "medusa-desktop-linux.AppImage": ("desktop-package", "linux", "x86_64", "x86_64-unknown-linux-gnu"),
    "medusa-desktop-macos-app.zip": ("desktop-package", "macos", "x86_64", "x86_64-apple-darwin"),
    "medusa-desktop-macos.dmg": ("desktop-package", "macos", "x86_64", "x86_64-apple-darwin"),
    "medusa-desktop-windows.exe": ("desktop-package", "windows", "x86_64", "x86_64-pc-windows-msvc"),
}


class EvidenceError(RuntimeError):
    """Raised when release evidence is incomplete, untrusted, or unsafe."""


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


def validate_hex(label: str, value: str, length: int) -> str:
    normalized = value.strip().lower()
    if len(normalized) != length or any(character not in "0123456789abcdef" for character in normalized):
        raise EvidenceError(f"{label} must contain {length} hexadecimal characters")
    return normalized


def generate_manifest(
    root: Path,
    assets: Path,
    output: Path,
    checksums: Path,
    version: str,
    revision: str,
    sequence: int,
    rollout_percentage: int,
    minimum_updater_version: str,
) -> dict:
    assets.mkdir(parents=True, exist_ok=True)
    excluded = {output, checksums, assets / SIGNATURE_NAME}
    files = safe_files(assets, excluded)
    validate_required_assets(files)
    revision = validate_hex("source revision", revision, 40)
    if sequence <= 0:
        raise EvidenceError("release sequence must be positive")
    if rollout_percentage < 1 or rollout_percentage > 100:
        raise EvidenceError("rollout percentage must be in 1..=100")

    evidence = [
        {
            "name": path.name,
            "bytes": path.stat().st_size,
            "sha256": sha256_file(path),
        }
        for path in files
    ]
    by_name = {entry["name"]: entry for entry in evidence}
    artifacts = []
    for name, (kind, operating_system, architecture, target) in INSTALLABLE_ASSETS.items():
        entry = by_name[name]
        artifacts.append(
            {
                "name": name,
                "kind": kind,
                "platform": {"os": operating_system, "architecture": architecture},
                "target": target,
                "bytes": entry["bytes"],
                "sha256": entry["sha256"],
            }
        )
    artifacts.sort(key=lambda entry: entry["name"])
    evidence.sort(key=lambda entry: entry["name"])
    manifest = {
        "schema": MANIFEST_SCHEMA,
        "version": version,
        "minimum_updater_version": minimum_updater_version,
        "source": {
            "repository": "benclawbot/Medusa",
            "revision": revision,
            "rust_toolchain": "1.88.0",
            "cargo_lock_sha256": sha256_file(root / "Cargo.lock"),
            "desktop_lock_sha256": sha256_file(root / "apps/medusa-desktop/package-lock.json"),
        },
        "rollout": {
            "channel": "stable",
            "sequence": sequence,
            "percentage": rollout_percentage,
        },
        "artifacts": artifacts,
        "evidence": evidence,
    }
    canonical = json.dumps(manifest, sort_keys=True, separators=(",", ":")) + "\n"
    output.write_text(canonical, encoding="utf-8")
    checksum_lines = [f"{entry['sha256']}  {entry['name']}" for entry in evidence]
    checksums.write_text("\n".join(checksum_lines) + "\n", encoding="utf-8")
    return manifest


def sign_manifest(manifest: Path, output: Path, private_key: Path, key_id: str) -> dict:
    if not key_id.strip():
        raise EvidenceError("release signing key ID is empty")
    if not private_key.is_file():
        raise EvidenceError(f"release signing key does not exist: {private_key}")
    mode = stat.S_IMODE(private_key.stat().st_mode)
    if os.name != "nt" and mode & 0o077:
        raise EvidenceError("release signing key must not be group/world accessible")
    with tempfile.TemporaryDirectory() as raw:
        signature_file = Path(raw) / "signature.bin"
        result = subprocess.run(
            [
                "openssl",
                "pkeyutl",
                "-sign",
                "-rawin",
                "-inkey",
                str(private_key),
                "-in",
                str(manifest),
                "-out",
                str(signature_file),
            ],
            check=False,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            raise EvidenceError(f"Ed25519 signing failed: {result.stderr.strip()}")
        signature = signature_file.read_bytes()
    if len(signature) != 64:
        raise EvidenceError(f"Ed25519 signature must contain 64 bytes, got {len(signature)}")
    envelope = {
        "schema": SIGNATURE_SCHEMA,
        "key_id": key_id,
        "algorithm": "Ed25519",
        "manifest_sha256": sha256_file(manifest),
        "signature": signature.hex(),
    }
    output.write_text(json.dumps(envelope, sort_keys=True, separators=(",", ":")) + "\n", encoding="utf-8")
    return envelope


def verify_signature(manifest: Path, signature: Path, public_key: Path) -> None:
    envelope = json.loads(signature.read_text(encoding="utf-8"))
    if envelope.get("schema") != SIGNATURE_SCHEMA or envelope.get("algorithm") != "Ed25519":
        raise EvidenceError("invalid release signature envelope")
    if envelope.get("manifest_sha256") != sha256_file(manifest):
        raise EvidenceError("release signature envelope has the wrong manifest digest")
    try:
        raw_signature = bytes.fromhex(str(envelope["signature"]))
    except (KeyError, ValueError) as error:
        raise EvidenceError("release signature is not valid hexadecimal") from error
    if len(raw_signature) != 64:
        raise EvidenceError("release signature has the wrong byte length")
    with tempfile.TemporaryDirectory() as raw:
        signature_file = Path(raw) / "signature.bin"
        signature_file.write_bytes(raw_signature)
        result = subprocess.run(
            [
                "openssl",
                "pkeyutl",
                "-verify",
                "-rawin",
                "-pubin",
                "-inkey",
                str(public_key),
                "-in",
                str(manifest),
                "-sigfile",
                str(signature_file),
            ],
            check=False,
            capture_output=True,
            text=True,
        )
    if result.returncode != 0:
        raise EvidenceError("release manifest Ed25519 verification failed")


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
        manifest = assets / "medusa-release-manifest.json"
        checksums = assets / "SHA256SUMS"
        first_manifest = generate_manifest(
            root, assets, manifest, checksums, "1.2.3", "a" * 40, 12, 100, "1.0.0"
        )
        first_bytes = manifest.read_bytes()
        second_manifest = generate_manifest(
            root, assets, manifest, checksums, "1.2.3", "a" * 40, 12, 100, "1.0.0"
        )
        assert first_manifest == second_manifest
        assert first_bytes == manifest.read_bytes()

        private_key = root / "private.pem"
        public_key = root / "public.pem"
        subprocess.run(
            ["openssl", "genpkey", "-algorithm", "Ed25519", "-out", str(private_key)],
            check=True,
            capture_output=True,
        )
        private_key.chmod(0o600)
        subprocess.run(
            ["openssl", "pkey", "-in", str(private_key), "-pubout", "-out", str(public_key)],
            check=True,
            capture_output=True,
        )
        signature = assets / SIGNATURE_NAME
        sign_manifest(manifest, signature, private_key, "fixture-key")
        verify_signature(manifest, signature, public_key)
        manifest.write_bytes(manifest.read_bytes() + b" ")
        try:
            verify_signature(manifest, signature, public_key)
        except EvidenceError:
            pass
        else:
            raise AssertionError("tampered manifest was accepted")

        duplicate = assets / "nested"
        duplicate.mkdir()
        (duplicate / "LICENSE").write_text("duplicate", encoding="utf-8")
        try:
            generate_manifest(
                root, assets, manifest, checksums, "1.2.3", "a" * 40, 12, 100, "1.0.0"
            )
        except EvidenceError:
            pass
        else:
            raise AssertionError("duplicate asset basename was accepted")

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
    manifest.add_argument("--root", type=Path, default=Path("."))
    manifest.add_argument("--assets", type=Path, required=True)
    manifest.add_argument("--output", type=Path, required=True)
    manifest.add_argument("--checksums", type=Path, required=True)
    manifest.add_argument("--version", required=True)
    manifest.add_argument("--revision", required=True)
    manifest.add_argument("--sequence", required=True, type=int)
    manifest.add_argument("--rollout-percentage", type=int, default=100)
    manifest.add_argument("--minimum-updater-version", required=True)

    sign = subcommands.add_parser("sign-manifest")
    sign.add_argument("--manifest", type=Path, required=True)
    sign.add_argument("--output", type=Path, required=True)
    sign.add_argument("--private-key", type=Path, required=True)
    sign.add_argument("--key-id", default=DEFAULT_KEY_ID)

    verify = subcommands.add_parser("verify-signature")
    verify.add_argument("--manifest", type=Path, required=True)
    verify.add_argument("--signature", type=Path, required=True)
    verify.add_argument("--public-key", type=Path, required=True)

    subcommands.add_parser("self-test")
    return result


def main() -> int:
    arguments = parser().parse_args()
    try:
        if arguments.command == "validate-tag":
            print(validate_tag(arguments.root, arguments.tag))
        elif arguments.command == "sbom":
            generate_sbom(arguments.root, arguments.output)
            print(arguments.output)
        elif arguments.command == "manifest":
            generate_manifest(
                arguments.root,
                arguments.assets,
                arguments.output,
                arguments.checksums,
                arguments.version,
                arguments.revision,
                arguments.sequence,
                arguments.rollout_percentage,
                arguments.minimum_updater_version,
            )
            print(arguments.output)
        elif arguments.command == "sign-manifest":
            sign_manifest(arguments.manifest, arguments.output, arguments.private_key, arguments.key_id)
            print(arguments.output)
        elif arguments.command == "verify-signature":
            verify_signature(arguments.manifest, arguments.signature, arguments.public_key)
            print("release-signature-ok")
        else:
            self_test()
    except (
        EvidenceError,
        KeyError,
        OSError,
        ValueError,
        subprocess.SubprocessError,
        tomllib.TOMLDecodeError,
    ) as error:
        print(f"release evidence error: {error}", file=os.sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
