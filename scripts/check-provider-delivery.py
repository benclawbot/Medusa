#!/usr/bin/env python3
"""Validate the public provider-delivery contract against shipped configuration."""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path


class ContractError(RuntimeError):
    pass


EXPECTED_CHOICES = ["chatgpt", "anthropic", "local", "custom"]
EXPECTED_CHECKS = [
    "authentication",
    "model_availability",
    "minimal_completion",
    "tool_use",
    "image_input",
    "context_window",
    "streaming",
    "external_dependencies",
]


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ContractError(message)


def validate(root: Path) -> None:
    contract = json.loads((root / "docs/provider-delivery-contract.json").read_text())
    require(contract.get("schema_version") == 1, "unsupported provider contract schema")
    require(contract.get("first_run_choices") == EXPECTED_CHOICES, "first-run choices drifted")
    require(contract.get("diagnostic_checks") == EXPECTED_CHECKS, "diagnostic checks drifted")
    require(contract.get("credential_persistence") is False, "credentials must not be persisted")
    require(
        contract.get("streaming_claim_policy")
        == "fail_closed_until_native_streaming_is_verified",
        "streaming claim policy must fail closed",
    )
    require(contract.get("custom_endpoint_requires_base_url") is True, "custom endpoints require base_url")

    diagnostic = (root / "crates/medusa-cli/src/provider_diagnostic.rs").read_text()
    for provider in ("openai", "openai-oauth", "anthropic", "minimax", "local", "custom"):
        require(f'"{provider}"' in diagnostic, f"diagnostic is missing provider {provider}")
    require("streaming" in diagnostic.lower(), "diagnostic is missing streaming validation")
    require("base_url" in diagnostic, "diagnostic is missing custom endpoint validation")

    docs = (root / "docs/PROVIDER-DELIVERY.md").read_text().lower()
    for label in ("chatgpt", "anthropic", "local", "advanced/custom"):
        require(label in docs, f"provider delivery documentation is missing {label}")
    require("credentials" in docs and "environment" in docs, "credential isolation is undocumented")

    for path in sorted((root / "examples").rglob("*.toml")):
        text = path.read_text()
        if re.search(r"(?m)^\s*streaming\s*=\s*true\s*$", text):
            raise ContractError(f"public example overclaims streaming support: {path.relative_to(root)}")


if __name__ == "__main__":
    try:
        validate(Path(".").resolve())
    except (ContractError, FileNotFoundError, json.JSONDecodeError) as error:
        print(f"provider-delivery-contract-error: {error}", file=sys.stderr)
        raise SystemExit(1)
    print("provider-delivery-contract-ok")
