#!/usr/bin/env python3
"""Validate and render the canonical provider/live-dogfood support matrix."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any


class ProviderSupportError(RuntimeError):
    """Raised when provider support declarations drift."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ProviderSupportError(message)


def load_manifest(root: Path) -> dict[str, Any]:
    path = root / "docs/provider-support.json"
    payload = json.loads(path.read_text(encoding="utf-8"))
    require(isinstance(payload, dict), "provider support manifest root must be an object")
    require(payload.get("schema_version") == 1, "unsupported provider support schema")
    providers = payload.get("providers")
    require(isinstance(providers, list) and providers, "providers must be a non-empty list")
    ids = [provider.get("id") for provider in providers if isinstance(provider, dict)]
    require(len(ids) == len(providers), "every provider entry must be an object with an id")
    require(len(ids) == len(set(ids)), "provider ids must be unique")

    support_tiers = set(payload.get("support_tiers", {}))
    dogfood_statuses = set(payload.get("dogfood_statuses", {}))
    primary = []
    for provider in providers:
        provider_id = provider["id"]
        require(provider.get("support_tier") in support_tiers, f"unknown support tier for {provider_id}")
        dogfood = provider.get("dogfood")
        require(isinstance(dogfood, dict), f"missing dogfood declaration for {provider_id}")
        require(dogfood.get("status") in dogfood_statuses, f"unknown dogfood status for {provider_id}")
        require(provider.get("realtime_voice") in {"unavailable", "external-acceptance-pending"},
                f"unknown realtime voice status for {provider_id}")
        if dogfood["status"] == "primary":
            primary.append(provider)

    require(len(primary) == 1, "exactly one provider must be the primary live dogfood route")
    selected = primary[0]
    for field in ("credential_environment", "default_model"):
        require(bool(selected.get(field)), f"primary dogfood provider requires {field}")
    for field in ("model", "protocol", "base_url", "auth"):
        require(bool(selected["dogfood"].get(field)), f"primary dogfood route requires {field}")
    return payload


def render_markdown(manifest: dict[str, Any]) -> str:
    lines = [
        "# Provider support authority",
        "",
        "This file is generated from `docs/provider-support.json`. The manifest is the reviewed support and live-dogfood authority; `medusa-config` tests keep the selectable Rust catalog synchronized with it.",
        "",
        "| Provider | Support tier | Runtime protocol | Credential | Live dogfood | Realtime voice |",
        "|---|---|---|---|---|---|",
    ]
    for provider in manifest["providers"]:
        credential = provider["credential_environment"] or "external/local route"
        lines.append(
            f"| `{provider['id']}` | `{provider['support_tier']}` | `{provider['runtime_protocol']}` | "
            f"`{credential}` | `{provider['dogfood']['status']}` | `{provider['realtime_voice']}` |"
        )
    lines.extend([
        "",
        "`production-supported` describes the selectable text/provider route; it does not promote a separate realtime or remote-frontend capability. Custom, managed, and local routes retain operator-owned endpoint dependencies.",
        "",
        "The scheduled cross-platform live dogfood gate resolves its provider, model, protocol, endpoint, authentication mode, and credential environment from the single `primary` entry. Other selectable routes remain configurable but are not represented as having passed that gate.",
        "",
        "## Quarantined live evidence",
        "",
    ])
    for capability in manifest["non_selectable_status"]["quarantined"]:
        lines.append(f"- `{capability['id']}`: {capability['reason']}")
    lines.extend([
        "",
        "The desktop application does not expose Realtime voice. ChatGPT OAuth remains a text-provider route because the Codex app-server does not provide the Realtime session credential required by a desktop microphone/WebRTC client.",
        "",
        "See `docs/LIVE-PROVIDER-DOGFOOD.md` for the bounded evidence contract and `docs/PROVIDER-DELIVERY.md` for first-run diagnostics.",
        "",
    ])
    return "\n".join(lines)


def validate_references(root: Path, manifest: dict[str, Any]) -> None:
    marker = "docs/provider-support.json"
    for relative in ("README.md", "docs/LIVE-PROVIDER-DOGFOOD.md", "docs/PROVIDER-DELIVERY.md"):
        require(marker in (root / relative).read_text(encoding="utf-8"), f"{relative} must link to {marker}")
    workflow = (root / ".github/workflows/live-provider-dogfood.yml").read_text(encoding="utf-8")
    primary = next(provider for provider in manifest["providers"] if provider["dogfood"]["status"] == "primary")
    credential = primary["credential_environment"]
    require(f"{credential}: ${{{{ secrets.{credential} }}}}" in workflow,
            "live dogfood workflow credential does not match the primary provider")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path("."))
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    root = args.root.resolve()
    try:
        manifest = load_manifest(root)
        rendered = render_markdown(manifest)
        output = root / "docs/PROVIDER-SUPPORT.md"
        if args.write:
            output.write_text(rendered, encoding="utf-8", newline="\n")
        else:
            require(output.read_text(encoding="utf-8") == rendered, "docs/PROVIDER-SUPPORT.md is stale")
        validate_references(root, manifest)
    except (ProviderSupportError, FileNotFoundError, json.JSONDecodeError) as error:
        print(f"provider-support-error: {error}", file=sys.stderr)
        return 1
    print("provider-support-ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
