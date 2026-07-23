#!/usr/bin/env python3
"""Adversarial fixtures for check-capability-evidence.py."""

from __future__ import annotations

import importlib.util
import json
import tempfile
from pathlib import Path

SCRIPT = Path(__file__).with_name("check-capability-evidence.py")
SPEC = importlib.util.spec_from_file_location("check_capability_evidence", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def write(root: Path, relative: str, content: str = "ok\n") -> None:
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def fixture(root: Path) -> Path:
    docs = [
        "README.md",
        "docs/CAPABILITY-EVIDENCE.md",
        "docs/RELEASE.md",
        "docs/COMPATIBILITY.md",
        "docs/REFACTOR-BASELINE.md",
        "docs/PUBLIC-API-BASELINE.md",
        "docs/BENCHMARKS.md",
    ]
    for document in docs:
        write(root, document)
    write(root, "README.md", "[Evidence](docs/CAPABILITY-EVIDENCE.md)\n")
    write(
        root,
        "docs/CAPABILITY-EVIDENCE.md",
        "# Ledger\n\n## Evidence rules\n\n## Shipped on `main`\n\n"
        "`sample-claim`\n\n## Canonical gates\n",
    )
    write(root, "src/lib.rs")
    write(root, "tests/sample.rs")
    manifest = {
        "schema_version": 1,
        "required_documents": docs,
        "claims": [
            {
                "id": "sample-claim",
                "status": "shipped",
                "summary": "sample",
                "production_paths": ["src/lib.rs"],
                "test_paths": ["tests/sample.rs"],
                "gates": ["CI"],
            }
        ],
    }
    path = root / "docs/CAPABILITY-CLAIMS.json"
    path.write_text(json.dumps(manifest), encoding="utf-8")
    return path


def expect_failure(root: Path, manifest: Path, expected: str) -> None:
    try:
        MODULE.validate(root, manifest)
    except MODULE.EvidenceError as error:
        assert expected in str(error), (expected, str(error))
    else:
        raise AssertionError(f"expected validation failure containing {expected!r}")


def main() -> int:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        manifest = fixture(root)
        MODULE.validate(root, manifest)

        (root / "src/lib.rs").unlink()
        expect_failure(root, manifest, "deleted or missing path")
        write(root, "src/lib.rs")

        write(root, "README.md", "Open PR #999 is tests passing.\n")
        expect_failure(root, manifest, "open PR state")
        write(root, "README.md", "[Evidence](docs/CAPABILITY-EVIDENCE.md)\n")

        payload = json.loads(manifest.read_text(encoding="utf-8"))
        payload["claims"][0]["gates"] = ["Imaginary Gate"]
        manifest.write_text(json.dumps(payload), encoding="utf-8")
        expect_failure(root, manifest, "unknown gates")

        payload["claims"][0]["gates"] = ["CI"]
        payload["claims"][0]["id"] = "missing-ledger-entry"
        manifest.write_text(json.dumps(payload), encoding="utf-8")
        expect_failure(root, manifest, "missing from docs/CAPABILITY-EVIDENCE.md")

    print("capability-evidence-fixtures-ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
