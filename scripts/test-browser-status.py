#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("check-browser-status.py")
SPEC = importlib.util.spec_from_file_location("check_browser_status", SCRIPT)
assert SPEC and SPEC.loader
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)


class Fixture:
    def __init__(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.write("docs/architecture/baseline.json", json.dumps({
            "capabilities": [["browser-tools", "preview", "certified-production", "preserve", "medusa-agent::ToolManager -> medusa-browserd", []]],
            "capability_paths": {"browser-tools": [
                "crates/medusa-capabilities", "crates/medusa-agent/src/tools", "crates/medusa-browser-client",
                "crates/medusa-browserd", "crates/medusa-agent/tests/browser_dispatch.rs",
                ".github/workflows/browser-dispatch-certification.yml"
            ]}
        }))
        self.write("README.md", "browser actions are readiness-gated preview\nMEDUSA_BROWSER_ENABLED MEDUSA_BROWSER_PATH MEDUSA_BROWSER_VERIFY_URL\n")
        self.write("docs/architecture/INDEX.md", "| Browser tools | preview | certified-production |\nreadiness-gated explicit opt-in medusa-agent::ToolManager medusa-browserd 0009-browser-preview-certification.md\n")
        self.write("docs/CONFIGURATION.md", "readiness-gated preview MEDUSA_BROWSER_ENABLED MEDUSA_BROWSER_PATH MEDUSA_BROWSER_VERIFY_URL MEDUSA_BROWSER_TIMEOUT_MS browser_evaluate\n")
        self.write("docs/architecture/decisions/0009-browser-preview-certification.md", "product status `preview` architecture status `certified-production` MEDUSA_BROWSER_ENABLED=true MEDUSA_BROWSER_TIMEOUT_MS browser_evaluate\n")
        self.write("crates/medusa-capabilities/src/registry.rs", "Capability::Browser MEDUSA_BROWSER_VERIFY_URL fn browser_capability_state() { /* browser model actions are explicitly disabled */ }\n")
        for path in (
            "crates/medusa-capabilities/.keep", "crates/medusa-agent/src/tools/.keep", "crates/medusa-browser-client/.keep",
            "crates/medusa-browserd/.keep", "crates/medusa-agent/tests/browser_dispatch.rs",
            ".github/workflows/browser-dispatch-certification.yml",
        ):
            self.write(path, "fixture\n")

    def write(self, relative: str, content: str) -> None:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")

    def close(self) -> None:
        self.temp.cleanup()


class BrowserStatusTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = Fixture()

    def tearDown(self) -> None:
        self.fixture.close()

    def test_consistent_preview_status_passes(self) -> None:
        CHECK.validate(self.fixture.root)

    def test_withheld_vs_production_mismatch_fails(self) -> None:
        self.fixture.write("README.md", "model-executable browser actions remain withheld\nMEDUSA_BROWSER_ENABLED MEDUSA_BROWSER_PATH MEDUSA_BROWSER_VERIFY_URL\n")
        with self.assertRaisesRegex(CHECK.BrowserStatusError, "contradicts runtime authority"):
            CHECK.validate(self.fixture.root)

    def test_quarantined_architecture_row_fails(self) -> None:
        self.fixture.write("docs/architecture/INDEX.md", "| Browser tools | withheld | quarantined | no executable projection until dispatcher |\n")
        with self.assertRaisesRegex(CHECK.BrowserStatusError, "contradicts runtime authority"):
            CHECK.validate(self.fixture.root)

    def test_missing_timeout_documentation_fails(self) -> None:
        self.fixture.write("docs/CONFIGURATION.md", "readiness-gated preview MEDUSA_BROWSER_ENABLED MEDUSA_BROWSER_PATH MEDUSA_BROWSER_VERIFY_URL browser_evaluate\n")
        with self.assertRaisesRegex(CHECK.BrowserStatusError, "configuration guide"):
            CHECK.validate(self.fixture.root)

    def test_deleted_accepted_adr_fails(self) -> None:
        (self.fixture.root / "docs/architecture/decisions/0009-browser-preview-certification.md").unlink()
        with self.assertRaises(CHECK.BrowserStatusError):
            CHECK.validate(self.fixture.root)


if __name__ == "__main__":
    unittest.main()
