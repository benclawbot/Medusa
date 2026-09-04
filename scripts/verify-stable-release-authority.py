#!/usr/bin/env python3
"""Re-download and verify the final signed stable-release draft before publication."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import tempfile
from pathlib import Path


AUTHORITY_ASSETS = {
    "medusa-release-manifest.json",
    "medusa-release-manifest.sig.json",
    "SHA256SUMS",
}


class VerificationError(RuntimeError):
    pass


def run(*args: str, capture: bool = False) -> str:
    result = subprocess.run(
        args,
        check=True,
        text=True,
        capture_output=capture,
        env=os.environ.copy(),
    )
    return result.stdout.strip() if capture else ""


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def load_json(path: Path) -> dict[str, object]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise VerificationError(f"expected JSON object: {path}")
    return value


def verify(tag: str, repo: str, root: Path, require_draft: bool) -> None:
    is_draft = run(
        "gh",
        "release",
        "view",
        tag,
        "--repo",
        repo,
        "--json",
        "isDraft",
        "--jq",
        ".isDraft",
        capture=True,
    )
    if require_draft and is_draft != "true":
        raise VerificationError(f"release {tag} is not a draft")

    with tempfile.TemporaryDirectory(prefix="medusa-release-verify-") as temp:
        release_dir = Path(temp) / "release"
        release_dir.mkdir()
        run("gh", "release", "download", tag, "--repo", repo, "--dir", str(release_dir))

        manifest_path = release_dir / "medusa-release-manifest.json"
        signature_path = release_dir / "medusa-release-manifest.sig.json"
        checksums_path = release_dir / "SHA256SUMS"
        for path in (manifest_path, signature_path, checksums_path):
            if not path.is_file() or path.stat().st_size == 0:
                raise VerificationError(f"missing final release authority asset: {path.name}")

        manifest_bytes = manifest_path.read_bytes()
        manifest = load_json(manifest_path)
        signature = load_json(signature_path)

        release_id = manifest.get("release_id")
        if release_id != tag.removeprefix("v"):
            raise VerificationError("downloaded manifest release_id does not match release tag")
        source = manifest.get("source")
        if not isinstance(source, dict):
            raise VerificationError("manifest source must be an object")
        if source.get("repository") != repo:
            raise VerificationError("manifest source repository does not match release repository")
        revision = source.get("revision")
        if not isinstance(revision, str) or len(revision) != 40:
            raise VerificationError("manifest source revision is not a full commit SHA")
        tag_revision = run(
            "gh",
            "api",
            f"repos/{repo}/commits/{tag}",
            "--jq",
            ".sha",
            capture=True,
        )
        if tag_revision != revision:
            raise VerificationError("release tag moved away from the manifest source revision")

        if signature.get("schema") != "medusa-release-signature-v1":
            raise VerificationError("unexpected release signature schema")
        if signature.get("algorithm") != "Ed25519":
            raise VerificationError("unexpected release signature algorithm")
        manifest_digest = hashlib.sha256(manifest_bytes).hexdigest()
        if signature.get("manifest_sha256") != manifest_digest:
            raise VerificationError("signature envelope does not bind downloaded manifest bytes")

        evidence = manifest.get("evidence")
        if not isinstance(evidence, list):
            raise VerificationError("manifest evidence must be an array")
        expected: dict[str, tuple[int, str]] = {}
        for item in evidence:
            if not isinstance(item, dict):
                raise VerificationError("manifest evidence entry must be an object")
            name = item.get("name")
            byte_count = item.get("bytes")
            digest = item.get("sha256")
            if not isinstance(name, str) or Path(name).name != name:
                raise VerificationError(f"invalid evidence asset name: {name!r}")
            if name in expected:
                raise VerificationError(f"duplicate evidence asset: {name}")
            if not isinstance(byte_count, int) or byte_count < 0:
                raise VerificationError(f"invalid byte count for {name}")
            if not isinstance(digest, str) or len(digest) != 64:
                raise VerificationError(f"invalid SHA-256 for {name}")
            expected[name] = (byte_count, digest)

        actual_names = {
            path.name
            for path in release_dir.iterdir()
            if path.is_file() and path.name not in AUTHORITY_ASSETS
        }
        if actual_names != set(expected):
            missing = sorted(set(expected) - actual_names)
            extra = sorted(actual_names - set(expected))
            raise VerificationError(f"manifest asset set mismatch: missing={missing}, extra={extra}")

        checksum_lines: list[str] = []
        for name in sorted(expected):
            path = release_dir / name
            expected_bytes, expected_digest = expected[name]
            if path.stat().st_size != expected_bytes:
                raise VerificationError(f"byte count mismatch for {name}")
            actual_digest = sha256(path)
            if actual_digest != expected_digest:
                raise VerificationError(f"SHA-256 mismatch for {name}")
            checksum_lines.append(f"{expected_digest}  {name}")

        provided_checksums = sorted(
            line.strip()
            for line in checksums_path.read_text(encoding="utf-8").splitlines()
            if line.strip()
        )
        if provided_checksums != sorted(checksum_lines):
            raise VerificationError("SHA256SUMS does not match final manifest evidence")

        key_id = signature.get("key_id")
        if not isinstance(key_id, str):
            raise VerificationError("signature key_id is missing")
        keyring = load_json(root / "release/keys/keyring.json")
        keys = keyring.get("keys")
        if not isinstance(keys, list):
            raise VerificationError("release keyring keys must be an array")
        key = next(
            (
                entry
                for entry in keys
                if isinstance(entry, dict)
                and entry.get("key_id") == key_id
                and entry.get("status") == "active"
            ),
            None,
        )
        if key is None:
            raise VerificationError(f"signature key is not active in keyring: {key_id}")
        public_key_file = key.get("public_key_file")
        if not isinstance(public_key_file, str):
            raise VerificationError(f"public key file missing for {key_id}")
        public_key = root / public_key_file
        if not public_key.is_file():
            raise VerificationError(f"public key file not found for {key_id}")

        signature_hex = signature.get("signature")
        if not isinstance(signature_hex, str):
            raise VerificationError("signature bytes are missing")
        try:
            signature_bytes = bytes.fromhex(signature_hex)
        except ValueError as error:
            raise VerificationError("signature is not valid hexadecimal") from error
        if len(signature_bytes) != 64:
            raise VerificationError("Ed25519 signature must be 64 bytes")
        raw_signature = Path(temp) / "signature.bin"
        raw_signature.write_bytes(signature_bytes)
        run(
            "openssl",
            "pkeyutl",
            "-verify",
            "-rawin",
            "-pubin",
            "-inkey",
            str(public_key),
            "-in",
            str(manifest_path),
            "-sigfile",
            str(raw_signature),
        )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--tag", required=True)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--root", type=Path, default=Path("."))
    parser.add_argument("--require-draft", action="store_true")
    args = parser.parse_args()
    try:
        verify(args.tag, args.repo, args.root.resolve(), args.require_draft)
    except (VerificationError, OSError, subprocess.CalledProcessError, json.JSONDecodeError) as error:
        print(f"stable-release-verification-error: {error}", file=os.sys.stderr)
        return 1
    print(f"stable-release-authority-ok:{args.tag}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
