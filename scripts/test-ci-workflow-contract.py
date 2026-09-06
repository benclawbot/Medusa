#!/usr/bin/env python3
"""Regression checks for workflow trust boundaries and authoritative CI gates."""

from __future__ import annotations

from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
WORKFLOWS = ROOT / ".github" / "workflows"


def read_workflow(name: str) -> str:
    return (WORKFLOWS / name).read_text(encoding="utf-8")


def test_publishers_require_authoritative_workspace_validation() -> None:
    ci = read_workflow("ci.yml")
    assert "workflow_call:" in ci
    assert "Full workspace target lint" in ci
    assert "Full workspace tests" in ci
    assert "Dependency policy" in ci

    rolling = read_workflow("rolling-main-cli.yml")
    assert "authoritative-validation:" in rolling
    assert "uses: ./.github/workflows/ci.yml" in rolling
    assert rolling.count("needs: authoritative-validation") == 2
    assert rolling.index("authoritative-validation:") < rolling.index("build-cli:")
    assert rolling.index("authoritative-validation:") < rolling.index("build-desktop:")

    stable = read_workflow("publish-release.yml")
    assert "authoritative-validation:" in stable
    assert "uses: ./.github/workflows/ci.yml" in stable
    assert "needs: authoritative-validation" in stable
    assert stable.index("authoritative-validation:") < stable.index("validate:")


def test_rolling_main_requires_independent_signatures() -> None:
    rolling = read_workflow("rolling-main-cli.yml")
    assert "sign-cli:" in rolling
    assert "sign-desktop:" in rolling
    assert rolling.count("environment: release-signing-primary") >= 2
    assert "MEDUSA_RELEASE_PRIMARY_ED25519_PRIVATE_KEY_PEM" in rolling
    assert "medusa-release-signature-v1" in rolling
    assert "rolling-main-cli-signatures-${{ github.sha }}" in rolling
    assert "rolling-main-desktop-signatures-${{ github.sha }}" in rolling
    assert "needs: [build-cli, sign-cli]" in rolling
    assert "needs: [build-desktop, sign-desktop, publish-cli]" in rolling
    assert "*.sig.json" in rolling

    source = (ROOT / "crates/medusa-update/src/source.rs").read_text(encoding="utf-8")
    assert "TrustStore::production()" in source
    assert "verify_detached(manifest_bytes, signature_bytes)" in source
    assert "format!(\"{manifest_name}.sig.json\")" in source
    assert "self.asset_base == ROLLING_ASSET_BASE" in source


def test_windows_distribution_requires_authenticode() -> None:
    stable = read_workflow("publish-release.yml")
    assert "Build and Authenticode-sign Windows release assets" in stable
    assert "environment: release-signing" in stable
    assert "WINDOWS_SIGNING_CERTIFICATE_BASE64" in stable
    assert "WINDOWS_SIGNING_CERTIFICATE_PASSWORD" in stable
    assert "npm run tauri:build -- --no-bundle" in stable
    assert "npm run tauri -- bundle --bundles nsis" in stable
    assert "https://timestamp.digicert.com" in stable
    assert stable.count("signtool.FullName verify /pa /all /v") >= 2
    assert stable.index("Authenticode-sign Windows product executables") < stable.index(
        "Bundle signed Windows desktop executable"
    )
    assert stable.index("Authenticode-sign Windows product executables") < stable.index(
        "Compress-Archive -Path cli-package/*"
    )

    # Rolling main is authenticated by the required Ed25519 updater authority.
    # It must not depend on stable-release platform credentials or a reviewed
    # platform-signing environment, otherwise every main push stops for approval
    # or fails when stable signing credentials are intentionally absent.
    rolling = read_workflow("rolling-main-cli.yml")
    assert "\n    environment: release-signing\n" not in rolling
    assert "WINDOWS_SIGNING_CERTIFICATE_BASE64" not in rolling
    assert "WINDOWS_SIGNING_CERTIFICATE_PASSWORD" not in rolling
    assert "Authenticode-sign rolling Windows CLI" not in rolling
    assert "Authenticode-sign rolling Windows desktop executable" not in rolling
    assert "Package exact-revision rolling CLI asset" in rolling
    assert "Package exact-revision desktop asset" in rolling


def test_stable_release_is_complete_before_publication() -> None:
    refresh = WORKFLOWS / "refresh-cli-release-assets.yml"
    assert not refresh.exists(), "stable release assets must never be refreshed after publication"

    publisher = read_workflow("publish-release.yml")
    assert publisher.count("--bin medusa-recall") == 3
    assert publisher.count("medusa-recall") >= 9
    assert 'gh release create "$RELEASE_TAG" release-assets/*' in publisher
    assert "--draft" in publisher
    assert "--clobber" not in publisher

    primary_name = "sign-release-manifest.yml"
    recovery_name = "sign-release-manifest-recovery.yml"
    verifier_path = "./.github/workflows/verify-published-release.yml"
    for name in (primary_name, recovery_name):
        signer = read_workflow(name)
        assert "--json isDraft --jq '.isDraft'" in signer
        assert "refusing to mutate published stable assets" in signer
        assert "--clobber" in signer
        assert 'gh release edit "$RELEASE_TAG" --repo "$GITHUB_REPOSITORY" --draft=false' in signer
        assert signer.index("gh release upload") < signer.index(
            'gh release edit "$RELEASE_TAG" --repo "$GITHUB_REPOSITORY" --draft=false'
        )
        assert verifier_path in signer
        assert signer.index('gh release edit "$RELEASE_TAG" --repo "$GITHUB_REPOSITORY" --draft=false') < signer.index(verifier_path)

    platform_signer = read_workflow("sign-draft-release.yml")
    assert "release $RELEASE_TAG must exist and remain a draft" in platform_signer
    assert "--clobber" in platform_signer
    assert platform_signer.index("is_draft=") < platform_signer.index("--clobber")

    # No other workflow that follows the normal stable Publish Release path may
    # overwrite release assets. Rolling-main publication is intentionally mutable
    # and is not a semver stable release.
    stable_followup_clobber_writers = []
    for path in WORKFLOWS.glob("*.yml"):
        text = path.read_text(encoding="utf-8")
        if 'workflows: ["Publish Release"]' in text and "--clobber" in text:
            stable_followup_clobber_writers.append(path.name)
    assert stable_followup_clobber_writers == [primary_name], stable_followup_clobber_writers

    verifier = read_workflow("verify-published-release.yml")
    for archive in (
        "medusa-cli-linux.tar.gz",
        "medusa-cli-macos.tar.gz",
        "medusa-cli-windows.zip",
    ):
        assert archive in verifier
    assert "update --check --release" in verifier
    assert "env -u GH_TOKEN -u GITHUB_TOKEN" in verifier
    assert "Remove-Item Env:GH_TOKEN" in verifier
    assert "Remove-Item Env:GITHUB_TOKEN" in verifier


def test_rolling_desktop_uses_tauri_production_build() -> None:
    # A raw Cargo build selects Tauri's devUrl and ships a desktop that requires Vite.
    workflow = read_workflow("rolling-main-cli.yml")
    assert "npm run tauri:build -- --no-bundle" in workflow
    assert "cargo build --release --locked --manifest-path" not in workflow
    assert "Smoke packaged Windows desktop origin" in workflow
    assert "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS" in workflow
    assert "localhost:5173" in workflow
    assert "tauri\\.localhost" in workflow
    assert "$observedUrls = @()" in workflow
    assert "never navigated to the Tauri production origin" in workflow
    assert workflow.index("Smoke packaged Windows desktop origin") < workflow.index(
        "Package exact-revision desktop asset"
    )


def test_pr_base_ref_is_bound_through_environment() -> None:
    workflow = read_workflow("ci.yml")
    assert 'git fetch --no-tags --depth=1 origin "${{ github.base_ref }}"' not in workflow
    assert workflow.count("BASE_REF: ${{ github.base_ref }}") == 2
    for line in workflow.splitlines():
        if "github.base_ref" in line:
            assert line.strip().startswith("BASE_REF:")


def test_secret_live_provider_pr_gate_is_same_repo() -> None:
    gate = "github.event.pull_request.head.repo.full_name == github.repository"
    for name in ("live-provider-dogfood.yml", "architecture-policy.yml"):
        assert gate in read_workflow(name)


def test_tui_model_is_explicitly_parameterized() -> None:
    wrapper = (ROOT / "scripts" / "run-live-tui-minimax-e2e.py").read_text(encoding="utf-8")
    assert '"--model", model' in wrapper
    assert "harness.write_profile =" not in wrapper


def test_openai_oauth_never_uses_latest() -> None:
    candidates = [ROOT / "README.md", ROOT / "docs", ROOT / "scripts", ROOT / ".github"]
    forbidden = "openai-oauth" + "@latest"
    pinned = "openai-oauth" + "@2.0.0"
    versioned = "openai-oauth" + "@"
    for candidate in candidates:
        paths = [candidate] if candidate.is_file() else candidate.rglob("*")
        for path in paths:
            if path.is_file() and path.suffix.lower() in {".md", ".json", ".py", ".ps1", ".yml", ".yaml", ".rs"}:
                text = path.read_text(encoding="utf-8")
                assert forbidden not in text, path
                for line in text.splitlines():
                    if versioned in line:
                        assert pinned in line, (path, line)


def main() -> int:
    tests = [
        test_publishers_require_authoritative_workspace_validation,
        test_rolling_main_requires_independent_signatures,
        test_windows_distribution_requires_authenticode,
        test_stable_release_is_complete_before_publication,
        test_rolling_desktop_uses_tauri_production_build,
        test_pr_base_ref_is_bound_through_environment,
        test_secret_live_provider_pr_gate_is_same_repo,
        test_tui_model_is_explicitly_parameterized,
        test_openai_oauth_never_uses_latest,
    ]
    for test in tests:
        test()
    print(f"CI workflow contract tests passed: {len(tests)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
