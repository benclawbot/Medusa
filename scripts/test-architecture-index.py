#!/usr/bin/env python3
"""Adversarial fixtures for the architecture-v2 drift checker."""

from __future__ import annotations

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("check-architecture-index.py")
SPEC = importlib.util.spec_from_file_location("check_architecture_index", SCRIPT)
assert SPEC and SPEC.loader
CHECKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECKER)


INDEX_SECTIONS = "\n".join(sorted(CHECKER.REQUIRED_INDEX_SECTIONS))
PR_TEXT = "\n".join(sorted(CHECKER.REQUIRED_PR_TEXT))
CODEOWNERS = "\n".join(f"{path} @owner" for path in sorted(CHECKER.REQUIRED_CODEOWNERS))


class Fixture:
    def __init__(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.write("Cargo.toml", '[workspace]\nresolver = "2"\nmembers = ["crates/medusa-core"]\n')
        self.write("crates/medusa-core/Cargo.toml", '[package]\nname = "medusa-core"\nversion = "0.0.0"\n')
        self.write(
            "docs/architecture/owners.json",
            json.dumps({"schema_version": 1, "owners": {"medusa-core": "foundation"}}),
        )
        for path in (
            "docs/ARCHITECTURE.md",
            "docs/CONTRIBUTOR-ARCHITECTURE.md",
            "docs/architecture/LEGACY-DELETION.md",
            "docs/architecture/RELEASE-POLICY.md",
        ):
            self.write(path, "# Fixture\n`crates/medusa-core`\n")
        self.write("docs/architecture/INDEX.md", f"# Fixture\n{INDEX_SECTIONS}\n")
        self.write("docs/architecture/decisions/0001-architecture-v2-reset.md", "# ADR\n")
        self.write(".github/PULL_REQUEST_TEMPLATE.md", PR_TEXT)
        self.write(".github/CODEOWNERS", CODEOWNERS)
        self.write("scripts/check-architecture-index.py", "# fixture\n")
        self.write("scripts/test-architecture-index.py", "# fixture\n")
        self.write("scripts/architecture-conformance.py", "# fixture\n")
        self.manifest = self.valid_manifest()
        self.save_manifest()

    def close(self) -> None:
        self.temp.cleanup()

    def write(self, relative: str, content: str) -> None:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")

    def save_manifest(self) -> None:
        self.write(
            "docs/architecture/baseline.json",
            json.dumps(self.manifest, indent=2),
        )

    @staticmethod
    def valid_manifest() -> dict[str, object]:
        migrations = [
            [issue, str(issue - 646), f"phase {issue}", "owner", ["contract"], ["consumer"], "delete legacy"]
            for issue in range(646, 656)
        ]
        fixtures = [
            [fixture_id, 600 + index, False, "probe", "remove when repaired"]
            for index, fixture_id in enumerate(sorted(CHECKER.REQUIRED_FIXTURES))
        ]
        return {
            "schema_version": 1,
            "baseline": {
                "issue": 646,
                "parent_issue": 645,
                "feature_freeze": {"active": True, "release_rule": "freeze"},
            },
            "deployment_modes": [
                ["headless", "medusa", "crates/medusa-core", "shared"]
            ],
            "components": {
                "rust_crates": {"medusa-core": "preserve"},
                "non_crate": [["governance", "docs/architecture", "preserve"]],
                "owner_groups": {"foundation": ["medusa-core"]},
            },
            "capabilities": [
                ["core", "production", "legacy-uncertified", "preserve", "dispatcher", []]
            ],
            "capability_paths": {"core": ["crates/medusa-core"]},
            "sources_of_truth": [
                ["session", "journal", [], "aggregate", "one authority"]
            ],
            "state_machines": [["execution-v2", ["plan", "complete"], "durable"]],
            "trust_boundaries": [["repository", "owner", ["confined"]]],
            "known_failure_fixtures": fixtures,
            "migration": migrations,
            "dependency_policy": {
                "forbidden_edges": [["crates/medusa-core", "medusa-runtime"]]
            },
            "governance": {
                "index": "docs/architecture/INDEX.md",
                "decision": "docs/architecture/decisions/0001-architecture-v2-reset.md",
                "pr_template": ".github/PULL_REQUEST_TEMPLATE.md",
                "codeowners": ".github/CODEOWNERS",
                "checker": "scripts/check-architecture-index.py",
                "conformance": "scripts/architecture-conformance.py",
                "release_policy": "docs/architecture/RELEASE-POLICY.md",
                "deletion_checklist": "docs/architecture/LEGACY-DELETION.md",
            },
        }


class ArchitectureIndexTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = Fixture()

    def tearDown(self) -> None:
        self.fixture.close()

    def validate(self) -> None:
        CHECKER.validate(self.fixture.root)

    def test_valid_fixture_passes(self) -> None:
        self.validate()

    def test_new_workspace_crate_requires_index_entry(self) -> None:
        self.fixture.write(
            "Cargo.toml",
            '[workspace]\nresolver = "2"\nmembers = ["crates/medusa-core", "crates/medusa-new"]\n',
        )
        self.fixture.write("crates/medusa-new/Cargo.toml", '[package]\nname="medusa-new"\nversion="0.0.0"\n')
        with self.assertRaisesRegex(CHECKER.ArchitectureIndexError, "workspace/index crate mismatch"):
            self.validate()

    def test_new_entrypoint_requires_real_implementation(self) -> None:
        self.fixture.manifest["deployment_modes"].append(
            ["ghost", "medusa ghost", "crates/medusa-ghost", "shared"]
        )
        self.fixture.save_manifest()
        with self.assertRaisesRegex(CHECKER.ArchitectureIndexError, "lacks implementation"):
            self.validate()

    def test_capability_requires_existing_production_path(self) -> None:
        self.fixture.manifest["capabilities"].append(
            ["ghost", "advertised", "quarantined", "replace", "missing", ["gap"]]
        )
        self.fixture.manifest["capability_paths"]["ghost"] = ["crates/medusa-ghost"]
        self.fixture.save_manifest()
        with self.assertRaisesRegex(CHECKER.ArchitectureIndexError, "references missing implementation"):
            self.validate()

    def test_duplicate_authority_is_rejected(self) -> None:
        self.fixture.manifest["sources_of_truth"].append(
            ["workers", "journal", [], "worker aggregate", "one authority"]
        )
        self.fixture.save_manifest()
        with self.assertRaisesRegex(CHECKER.ArchitectureIndexError, "duplicate current authority"):
            self.validate()

    def test_forbidden_dependency_is_rejected(self) -> None:
        self.fixture.write(
            "crates/medusa-core/Cargo.toml",
            '[package]\nname="medusa-core"\nversion="0.0.0"\n[dependencies]\nmedusa-runtime = "1"\n',
        )
        with self.assertRaisesRegex(CHECKER.ArchitectureIndexError, "forbidden dependency present"):
            self.validate()

    def test_unknown_documented_component_is_rejected(self) -> None:
        self.fixture.write(
            "docs/CONTRIBUTOR-ARCHITECTURE.md",
            "# Fixture\n`crates/medusa-does-not-exist`\n",
        )
        with self.assertRaisesRegex(CHECKER.ArchitectureIndexError, "unknown crates/components"):
            self.validate()


if __name__ == "__main__":
    unittest.main()
