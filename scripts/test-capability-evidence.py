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


def claim(claim_id: str = "sample-claim", maturity: str = "production") -> dict[str, object]:
    production = maturity == "production"
    return {
        "id": claim_id,
        "maturity": maturity,
        "summary": "sample",
        "owner": "sample maintainers",
        "production_paths": ["src/lib.rs"],
        "test_paths": ["tests/sample.rs"],
        "gates": ["CI"],
        "entrypoints": ["sample"] if maturity != "design-only" else [],
        "supported_platforms": ["linux"],
        "external_dependencies": [],
        "observability": ["docs/OBSERVABILITY.md"],
        "documentation": ["README.md"],
        "promotion_checklist": sorted(MODULE.PRODUCTION_CHECKLIST) if production else [],
        "default_enabled": production,
        "opt_in": None if maturity in {"production", "design-only"} else "--enable-sample",
        "dependencies": [],
    }


def fixture(root: Path) -> Path:
    docs = [
        "README.md",
        "docs/ARCHITECTURE.md",
        "docs/CONTRIBUTOR-ARCHITECTURE.md",
        "docs/CAPABILITY-EVIDENCE.md",
        "docs/RELEASE.md",
        "docs/COMPATIBILITY.md",
        "docs/REFACTOR-BASELINE.md",
        "docs/PUBLIC-API-BASELINE.md",
        "docs/BENCHMARKS.md",
        "docs/OBSERVABILITY.md",
    ]
    for document in docs:
        write(root, document)
    write(root, "README.md", "[Evidence](docs/CAPABILITY-EVIDENCE.md)\n")
    write(
        root,
        "docs/CAPABILITY-EVIDENCE.md",
        "# Ledger\n\n## Evidence rules\n\n## Capability maturity matrix\n\n"
        "`sample-claim` `production` `preview` `experimental` `design-only`\n\n"
        "## Canonical gates\n",
    )
    write(root, "src/lib.rs")
    write(root, "tests/sample.rs")
    manifest = {
        "schema_version": 2,
        "maturity_model": {
            "production": "production",
            "preview": "preview",
            "experimental": "experimental",
            "design-only": "design-only",
        },
        "required_documents": docs,
        "claims": [claim()],
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


def load(manifest: Path) -> dict[str, object]:
    return json.loads(manifest.read_text(encoding="utf-8"))


def save(manifest: Path, payload: dict[str, object]) -> None:
    manifest.write_text(json.dumps(payload), encoding="utf-8")


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

        payload = load(manifest)
        payload["claims"][0]["gates"] = ["Imaginary Gate"]
        save(manifest, payload)
        expect_failure(root, manifest, "unknown gates")

        payload["claims"][0]["gates"] = ["CI"]
        payload["claims"][0]["promotion_checklist"] = []
        save(manifest, payload)
        expect_failure(root, manifest, "incomplete promotion checklist")

        payload["claims"][0] = claim("preview-default", "preview")
        payload["claims"][0]["default_enabled"] = True
        save(manifest, payload)
        expect_failure(root, manifest, "must not be default enabled")

        payload["claims"] = [claim("design-entrypoint", "design-only")]
        payload["claims"][0]["entrypoints"] = ["medusa"]
        save(manifest, payload)
        expect_failure(root, manifest, "must not expose production entrypoints")

        payload["claims"] = [claim("production-parent"), claim("research-child", "design-only")]
        payload["claims"][0]["dependencies"] = ["research-child"]
        ledger = root / "docs/CAPABILITY-EVIDENCE.md"
        ledger.write_text(ledger.read_text(encoding="utf-8") + "`production-parent` `research-child`\n", encoding="utf-8")
        save(manifest, payload)
        expect_failure(root, manifest, "depends on non-production capability")

    print("capability-evidence-fixtures-ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
