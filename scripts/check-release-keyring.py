#!/usr/bin/env python3
"""Reject release keyrings that cannot satisfy the documented rotation policy."""

from __future__ import annotations

import base64
import json
import re
import sys
from pathlib import Path
from typing import Any


ED25519_SPKI_PREFIX = bytes.fromhex("302a300506032b6570032100")
ACTIVE_ROLES = {"primary", "recovery"}
SIGNING_WORKFLOWS = {
    "primary": ".github/workflows/sign-release-manifest.yml",
    "recovery": ".github/workflows/sign-release-manifest-recovery.yml",
}


class KeyringError(RuntimeError):
    """Raised when release trust state is incomplete or internally inconsistent."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise KeyringError(message)


def public_key_bytes(path: Path) -> bytes:
    text = path.read_text(encoding="ascii")
    match = re.fullmatch(
        r"-----BEGIN PUBLIC KEY-----\s+([A-Za-z0-9+/=\s]+)-----END PUBLIC KEY-----\s*",
        text,
    )
    require(match is not None, f"invalid public-key PEM: {path}")
    der = base64.b64decode("".join(match.group(1).split()), validate=True)
    require(der.startswith(ED25519_SPKI_PREFIX) and len(der) == 44, f"not an Ed25519 public key: {path}")
    return der[-32:]


def validate_keyring(root: Path, payload: dict[str, Any]) -> None:
    require(payload.get("schema") == "medusa-release-keyring-v1", "unsupported keyring schema")
    keys = payload.get("keys")
    require(isinstance(keys, list) and keys, "keyring keys must be a non-empty list")
    policy = payload.get("rotation_policy")
    require(isinstance(policy, dict), "missing rotation policy")
    minimum_overlap = policy.get("minimum_overlap_releases")
    require(isinstance(minimum_overlap, int) and minimum_overlap >= 2, "minimum overlap must be at least two releases")

    ids: set[str] = set()
    files: set[str] = set()
    active: dict[str, dict[str, Any]] = {}
    secrets: set[str] = set()
    for key in keys:
        require(isinstance(key, dict), "key entries must be objects")
        key_id = key.get("key_id")
        require(isinstance(key_id, str) and key_id and key_id not in ids, "key ids must be unique")
        ids.add(key_id)
        status = key.get("status")
        require(status in {"active", "revoked"}, f"invalid status for {key_id}")
        first = key.get("first_sequence")
        last = key.get("last_sequence")
        require(isinstance(first, int) and first > 0, f"invalid first sequence for {key_id}")
        require(last is None or isinstance(last, int) and last >= first, f"invalid last sequence for {key_id}")
        public_file = key.get("public_key_file")
        require(isinstance(public_file, str) and public_file not in files, f"public-key files must be unique for {key_id}")
        files.add(public_file)
        raw = public_key_bytes(root / public_file)
        require(raw.hex() == key.get("public_key_hex"), f"public key hex does not match {public_file}")

        role = key.get("role")
        secret = key.get("private_key_secret")
        if status == "active":
            require(role in ACTIVE_ROLES and role not in active, f"active key has invalid or duplicate role: {key_id}")
            require(isinstance(secret, str) and secret and secret not in secrets, f"active key requires a unique secret: {key_id}")
            secrets.add(secret)
            active[role] = key
        else:
            require(secret is None, f"revoked key must not retain a secret reference: {key_id}")

    require(set(active) == ACTIVE_ROLES, "one active primary and one active recovery key are required")
    primary = active["primary"]
    recovery = active["recovery"]
    overlap_start = max(primary["first_sequence"], recovery["first_sequence"])
    ends = [end for end in (primary["last_sequence"], recovery["last_sequence"]) if end is not None]
    if len(ends) == 2:
        require(min(ends) - overlap_start + 1 >= minimum_overlap, "active key windows do not satisfy overlap policy")


def validate_references(root: Path, payload: dict[str, Any]) -> None:
    workflows = {
        role: (root / relative).read_text(encoding="utf-8")
        for role, relative in SIGNING_WORKFLOWS.items()
    }
    rust = (root / "crates/medusa-update/src/manifest.rs").read_text(encoding="utf-8")
    active = [key for key in payload["keys"] if key["status"] == "active"]
    for key in active:
        role = key["role"]
        workflow = workflows[role]
        for value in (key["key_id"], key["private_key_secret"]):
            require(value in workflow, f"{role} signing workflow does not reference {value}")

        raw_public_key = bytes.fromhex(key["public_key_hex"])
        spki_base64 = base64.b64encode(ED25519_SPKI_PREFIX + raw_public_key).decode("ascii")
        require(
            spki_base64 in workflow,
            f"{role} signing workflow does not embed the public key from {key['public_key_file']}",
        )
        require(key["key_id"] in rust, f"updater trust store does not reference {key['key_id']}")

    for relative in ("docs/RELEASE-SIGNING.md", "docs/architecture/PREBUILT-UPDATES.md", "docs/RELEASE.md"):
        text = (root / relative).read_text(encoding="utf-8")
        require("release/keys/keyring.json" in text, f"{relative} must link to the keyring authority")


def validate(root: Path) -> None:
    payload = json.loads((root / "release/keys/keyring.json").read_text(encoding="utf-8"))
    require(isinstance(payload, dict), "keyring root must be an object")
    validate_keyring(root, payload)
    validate_references(root, payload)


if __name__ == "__main__":
    try:
        validate(Path(".").resolve())
    except (KeyringError, FileNotFoundError, UnicodeError, ValueError, json.JSONDecodeError) as error:
        print(f"release-keyring-error: {error}", file=sys.stderr)
        raise SystemExit(1)
    print("release-keyring-ok")
