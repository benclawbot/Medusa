#!/usr/bin/env python3
"""Apply the large-file integration edits for issue 655, then disappear."""

from __future__ import annotations

import re
from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    if text.count(old) != 1:
        raise RuntimeError(f"{label}: expected one match, found {text.count(old)}")
    return text.replace(old, new, 1)


def update_cli() -> None:
    path = Path("crates/medusa-cli/src/main.rs")
    text = path.read_text(encoding="utf-8")
    text = replace_once(
        text,
        "mod headless_approval;\n",
        "mod headless_approval;\nmod update_command;\n",
        "CLI module declaration",
    )
    pattern = re.compile(
        r"    /// Check for or install the latest Medusa main-branch build\.\n"
        r"    Update \{\n"
        r"(?:.*\n)*?"
        r"    \},\n"
        r"    Search \{",
    )
    replacement = '''    /// Check for or install a verified prebuilt Medusa release.
    Update {
        /// Report whether an eligible update exists without modifying this installation.
        #[arg(long)]
        check: bool,
        /// Apply an available update without an additional prompt (for managed automation).
        #[arg(long)]
        automatic: bool,
        /// Select the verified release channel or the explicit slow developer source channel.
        #[arg(long, default_value = "release", value_parser = ["release", "source"])]
        channel: String,
        /// Permit an intentional version or rollout-sequence rollback.
        #[arg(long)]
        allow_downgrade: bool,
    },
    Search {'''
    text, count = pattern.subn(replacement, text, count=1)
    if count != 1:
        raise RuntimeError("CLI update command block changed unexpectedly")
    text = replace_once(
        text,
        "fn run() -> MedusaResult<()> {\n    let cli = Cli::parse();",
        "fn run() -> MedusaResult<()> {\n    medusa_update::acknowledge_update_health()?;\n    let cli = Cli::parse();",
        "CLI startup health acknowledgement",
    )
    text = replace_once(
        text,
        "        CommandKind::Update { check, automatic } => update(&repo, check, automatic),",
        '''        CommandKind::Update {
            check,
            automatic,
            channel,
            allow_downgrade,
        } => update_command::run(&repo, check, automatic, &channel, allow_downgrade),''',
        "CLI update dispatch",
    )
    text = replace_once(
        text,
        "use medusa_update::{InstallKind, InstallLocation, MainBranchUpdater, UpdatePolicy};\n",
        "",
        "legacy updater import",
    )
    start = text.find("\nfn update(repo: &Path, check_only: bool, automatic: bool) -> MedusaResult<()> {")
    end = text.find("\nfn request_daemon_shutdown(", start)
    if start < 0 or end < 0:
        raise RuntimeError("legacy update function boundaries changed unexpectedly")
    text = text[:start] + "\n" + text[end:]
    path.write_text(text, encoding="utf-8")


def update_release_workflow() -> None:
    path = Path(".github/workflows/publish-release.yml")
    text = path.read_text(encoding="utf-8")
    text = replace_once(
        text,
        '''  publish-draft:
    name: Attest and create release
    needs: [validate, linux, macos, windows]
    runs-on: ubuntu-latest
    timeout-minutes: 20''',
        '''  publish-draft:
    name: Attest and create release
    needs: [validate, linux, macos, windows]
    runs-on: ubuntu-latest
    environment: release-signing
    timeout-minutes: 20''',
        "release-signing environment",
    )
    text = replace_once(
        text,
        '''          RELEASE_TAG: ${{ needs.validate.outputs.release-tag }}
          RELEASE_SHA: ${{ github.sha }}''',
        '''          RELEASE_TAG: ${{ needs.validate.outputs.release-tag }}
          RELEASE_VERSION: ${{ needs.validate.outputs.version }}
          RELEASE_SHA: ${{ github.sha }}
          RELEASE_SEQUENCE: ${{ github.run_number }}
          MEDUSA_RELEASE_ED25519_PRIVATE_KEY_PEM: ${{ secrets.MEDUSA_RELEASE_ED25519_PRIVATE_KEY_PEM }}''',
        "release signing environment variables",
    )
    text = replace_once(
        text,
        '''          python3 scripts/release-evidence.py manifest \\
            --assets release-assets \\
            --output release-assets/medusa-release-manifest.json \\
            --checksums release-assets/SHA256SUMS''',
        '''          release_key="$RUNNER_TEMP/medusa-release-ed25519.pem"
          test -n "$MEDUSA_RELEASE_ED25519_PRIVATE_KEY_PEM"
          umask 077
          printf '%s\\n' "$MEDUSA_RELEASE_ED25519_PRIVATE_KEY_PEM" > "$release_key"
          python3 scripts/release-evidence.py manifest \\
            --root . \\
            --assets release-assets \\
            --output release-assets/medusa-release-manifest.json \\
            --checksums release-assets/SHA256SUMS \\
            --version "$RELEASE_VERSION" \\
            --revision "$RELEASE_SHA" \\
            --sequence "$RELEASE_SEQUENCE" \\
            --rollout-percentage 100 \\
            --minimum-updater-version "1.0.0"
          python3 scripts/release-evidence.py sign-manifest \\
            --manifest release-assets/medusa-release-manifest.json \\
            --output release-assets/medusa-release-manifest.sig.json \\
            --private-key "$release_key" \\
            --key-id medusa-release-2026-01
          python3 scripts/release-evidence.py verify-signature \\
            --manifest release-assets/medusa-release-manifest.json \\
            --signature release-assets/medusa-release-manifest.sig.json \\
            --public-key release/keys/medusa-release-2026-01.pem
          rm -f "$release_key"''',
        "release manifest generation",
    )
    path.write_text(text, encoding="utf-8")


if __name__ == "__main__":
    update_cli()
    update_release_workflow()
