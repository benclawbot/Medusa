#!/usr/bin/env python3
"""Apply issue 655 architecture-index and baseline updates."""

from __future__ import annotations

import json
from pathlib import Path


BASELINE = Path("docs/architecture/baseline.json")
INDEX = Path("docs/architecture/INDEX.md")
CODEOWNERS = Path(".github/CODEOWNERS")


def replace_once(text: str, old: str, new: str, label: str) -> str:
    if text.count(old) != 1:
        raise RuntimeError(f"{label}: expected one match, found {text.count(old)}")
    return text.replace(old, new, 1)


def update_baseline() -> None:
    data = json.loads(BASELINE.read_text(encoding="utf-8"))

    for row in data["deployment_modes"]:
        if row[0] == "self-update":
            row[3] = "Ed25519-verified prebuilt release; explicit source developer channel"
            break
    else:
        raise RuntimeError("self-update deployment mode missing")

    data["components"]["rust_crates"]["medusa-update"] = "preserve"

    replacements = {
        "release-trust": [
            "release-trust",
            "production",
            "certified-production",
            "preserve",
            "signed release manifest v2 and protected release-signing workflow",
            [],
        ],
        "self-update": [
            "self-update",
            "production",
            "certified-production",
            "preserve",
            "medusa update verified release channel",
            [],
        ],
    }
    for index, row in enumerate(data["capabilities"]):
        if row[0] in replacements:
            data["capabilities"][index] = replacements.pop(row[0])
    if replacements:
        raise RuntimeError(f"capability rows missing: {sorted(replacements)}")

    data["capability_paths"]["release-trust"] = [
        ".github/workflows/publish-release.yml",
        ".github/workflows/sign-release-manifest.yml",
        "scripts/release-evidence.py",
        "release/keys",
    ]
    data["capability_paths"]["self-update"] = [
        "crates/medusa-update",
        "crates/medusa-cli/src/update_command.rs",
        "docs/architecture/PREBUILT-UPDATES.md",
    ]

    for row in data["sources_of_truth"]:
        if row[0] == "updates and releases":
            row[:] = [
                "updates and releases",
                "signed release manifest v2 plus protected release-signing workflow",
                ["GitHub release metadata", "package-manager state", "source developer channel"],
                "Ed25519-verified prebuilt manifest and embedded reviewed keyring",
                "updates verify signature before metadata and never silently compile source",
            ]
            break
    else:
        raise RuntimeError("updates and releases source-of-truth row missing")

    if not any(row[0] == "update-v2" for row in data["state_machines"]):
        data["state_machines"].append(
            [
                "update-v2",
                [
                    "check",
                    "verify-manifest",
                    "download",
                    "verify-artifact",
                    "extract-confined",
                    "stage",
                    "restart-handoff",
                    "healthy-or-rollback",
                ],
                "the previous executable is retained until the replacement acknowledges startup",
            ]
        )

    for row in data["trust_boundaries"]:
        if row[0] == "release-update":
            row[:] = [
                "release-update",
                "release maintainers and medusa-update trust store",
                [
                    "Ed25519 signature before manifest parsing",
                    "exact OS and architecture selection",
                    "signed size and SHA-256",
                    "confined extraction",
                    "health-checked atomic rollback",
                    "explicit downgrade and source-channel approval",
                ],
            ]
            break
    else:
        raise RuntimeError("release-update trust boundary missing")

    for row in data["migration"]:
        if row[0] == 655:
            row[:] = [
                655,
                "0-companion",
                "verified prebuilt updater",
                "release",
                [
                    "signed manifest v2",
                    "release keyring",
                    "update phase diagnostics",
                    "health-checked replacement",
                ],
                ["CLI", "release workflows", "Linux", "macOS", "Windows"],
                "default main-branch source-build updater",
            ]
            break
    else:
        data["migration"].append(
            [
                655,
                "0-companion",
                "verified prebuilt updater",
                "release",
                [
                    "signed manifest v2",
                    "release keyring",
                    "update phase diagnostics",
                    "health-checked replacement",
                ],
                ["CLI", "release workflows", "Linux", "macOS", "Windows"],
                "default main-branch source-build updater",
            ]
        )

    BASELINE.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")


def update_index() -> None:
    text = INDEX.read_text(encoding="utf-8")
    text = replace_once(
        text,
        "| Update | `medusa update` | `crates/medusa-update` | legacy source-build path pending #655 |",
        "| Update | `medusa update` | `crates/medusa-update` | Ed25519-verified prebuilt release; explicit `--channel source` developer path |",
        "deployment row",
    )
    text = replace_once(
        text,
        "| Release trust | production | legacy-uncertified | adapt | connect #655 verified artifacts to updater |",
        "| Release trust | production | certified-production | preserve | signed manifest v2, protected signer, reviewed keyring, and CI artifacts |",
        "release trust capability",
    )
    text = replace_once(
        text,
        "| Self-update | production | quarantined | replace | source compilation is the default channel |",
        "| Self-update | production | certified-production | preserve | verified prebuilt release is default; source compilation is explicit and never a fallback |",
        "self-update capability",
    )
    marker = "## Known-failure compatibility fixtures\n"
    section = '''## Verified release and update authority

The architecture and state machine are defined in [`PREBUILT-UPDATES.md`](PREBUILT-UPDATES.md) and ADR [`0002-verified-prebuilt-updates.md`](decisions/0002-verified-prebuilt-updates.md). The stable updater verifies an embedded Ed25519 key before trusting manifest metadata, selects one exact OS/architecture archive, verifies signed size and SHA-256, confines extraction, stages beside the running executable, and retains the previous binary until startup acknowledgement. The protected signer consumes only artifacts already produced by release CI. Unsigned releases, unknown or revoked keys, wrong-platform assets, traversal, tampering, truncation, version or rollout rollback, and startup failure fail closed.

`medusa update --channel source` is the sole source-build path. It is an explicit developer exception and is never selected automatically or used as a fallback.

'''
    if section not in text:
        text = replace_once(text, marker, section + marker, "verified update section")
    text = replace_once(
        text,
        "The indexed boundaries are repository mutation, platform containment, unsafe/FFI, secrets, provider network, GitHub OAuth/API, browser sidecar, plugins, and release/update artifacts. #653 and #655 are phase-0 companions and remain allowed during the freeze because they close trust and distribution boundaries rather than expand product scope.",
        "The indexed boundaries are repository mutation, platform containment, unsafe/FFI, secrets, provider network, GitHub OAuth/API, browser sidecar, plugins, and release/update artifacts. #653 and #655 are phase-0 companions: #653 closes the native FFI boundary and #655 closes the signed distribution and rollback boundary without expanding product scope.",
        "trust boundary summary",
    )
    text = replace_once(
        text,
        "- Decision: [`decisions/0001-architecture-v2-reset.md`](decisions/0001-architecture-v2-reset.md)\n",
        "- Decision: [`decisions/0001-architecture-v2-reset.md`](decisions/0001-architecture-v2-reset.md)\n- Decision: [`decisions/0002-verified-prebuilt-updates.md`](decisions/0002-verified-prebuilt-updates.md)\n- Verified update architecture: [`PREBUILT-UPDATES.md`](PREBUILT-UPDATES.md)\n",
        "decision index",
    )
    INDEX.write_text(text, encoding="utf-8")


def update_codeowners() -> None:
    text = CODEOWNERS.read_text(encoding="utf-8")
    block = '''
# Verified release and update authority
/release/keys/ @benclawbot
/docs/architecture/PREBUILT-UPDATES.md @benclawbot
/docs/architecture/decisions/0002-verified-prebuilt-updates.md @benclawbot
/docs/RELEASE.md @benclawbot
/docs/RELEASE-SIGNING.md @benclawbot
/scripts/release-evidence.py @benclawbot
/.github/workflows/verified-prebuilt-update.yml @benclawbot
/.github/workflows/sign-release-manifest.yml @benclawbot
'''
    if "# Verified release and update authority" not in text:
        text = text.replace("\n# Foundational and production authority boundaries\n", block + "\n# Foundational and production authority boundaries\n", 1)
    CODEOWNERS.write_text(text, encoding="utf-8")


if __name__ == "__main__":
    update_baseline()
    update_index()
    update_codeowners()
