#!/usr/bin/env python3
"""Regression tests for the exact-revision rolling update bundle validator."""

from __future__ import annotations

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "verify_main_update_bundle", ROOT / "scripts/verify-main-update-bundle.py"
)
assert SPEC and SPEC.loader
CHECKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECKER)

REVISION = "0123456789abcdef0123456789abcdef01234567"


class MainUpdateBundleTests(unittest.TestCase):
    def write_bundle(self, root: Path, revision: str = REVISION) -> None:
        for name in CHECKER.EXPECTED_ARCHIVES.values():
            archive = root / name
            archive.write_bytes((name.encode("utf-8") * 32)[:1024])
            manifest = {
                "bytes": archive.stat().st_size,
                "name": name,
                "revision": revision,
                "schema": CHECKER.SCHEMA,
                "sha256": CHECKER.file_digest(archive),
            }
            (root / f"{name}.json").write_text(
                json.dumps(manifest, sort_keys=True), encoding="utf-8"
            )

    def test_complete_bundle_is_accepted(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.write_bundle(root)
            self.assertEqual(
                set(CHECKER.verify_bundle(root, REVISION)),
                set(CHECKER.EXPECTED_ARCHIVES.values()),
            )

    def test_stale_manifest_revision_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.write_bundle(root, "fedcba9876543210fedcba9876543210fedcba98")
            with self.assertRaisesRegex(ValueError, "revision"):
                CHECKER.verify_bundle(root, REVISION)

    def test_missing_platform_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.write_bundle(root)
            missing = next(iter(CHECKER.EXPECTED_ARCHIVES.values()))
            (root / missing).unlink()
            (root / f"{missing}.json").unlink()
            with self.assertRaisesRegex(ValueError, "exactly"):
                CHECKER.verify_bundle(root, REVISION)

    def test_digest_mismatch_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.write_bundle(root)
            name = next(iter(CHECKER.EXPECTED_ARCHIVES.values()))
            manifest_path = root / f"{name}.json"
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            manifest["sha256"] = "0" * 64
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "digest mismatch"):
                CHECKER.verify_bundle(root, REVISION)


if __name__ == "__main__":
    unittest.main()
