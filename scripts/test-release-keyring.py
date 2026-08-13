#!/usr/bin/env python3
"""Adversarial fixtures for release keyring policy validation."""

from __future__ import annotations

import copy
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("release_keyring", ROOT / "scripts/check-release-keyring.py")
assert SPEC and SPEC.loader
CHECKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECKER)


class ReleaseKeyringPolicyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.payload = json.loads((ROOT / "release/keys/keyring.json").read_text(encoding="utf-8"))

    def validate_payload(self, payload: dict) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            for key in self.payload["keys"]:
                source = ROOT / key["public_key_file"]
                target = root / key["public_key_file"]
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(source.read_bytes())
            CHECKER.validate_keyring(root, payload)

    def test_current_keyring_is_rotation_ready(self) -> None:
        self.validate_payload(copy.deepcopy(self.payload))

    def test_single_active_authority_is_rejected(self) -> None:
        payload = copy.deepcopy(self.payload)
        next(key for key in payload["keys"] if key["role"] == "recovery")["status"] = "revoked"
        next(key for key in payload["keys"] if key["role"] == "recovery")["private_key_secret"] = None
        with self.assertRaises(CHECKER.KeyringError):
            self.validate_payload(payload)

    def test_duplicate_secret_custody_is_rejected(self) -> None:
        payload = copy.deepcopy(self.payload)
        primary = next(key for key in payload["keys"] if key["role"] == "primary")
        recovery = next(key for key in payload["keys"] if key["role"] == "recovery")
        recovery["private_key_secret"] = primary["private_key_secret"]
        with self.assertRaises(CHECKER.KeyringError):
            self.validate_payload(payload)

    def test_public_key_mismatch_is_rejected(self) -> None:
        payload = copy.deepcopy(self.payload)
        payload["keys"][0]["public_key_hex"] = "00" * 32
        with self.assertRaises(CHECKER.KeyringError):
            self.validate_payload(payload)


if __name__ == "__main__":
    unittest.main()
