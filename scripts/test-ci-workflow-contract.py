#!/usr/bin/env python3
"""Regression checks for workflow trust boundaries and authoritative CI gates."""

from __future__ import annotations

from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def read_workflow(name: str) -> str:
    return (ROOT / ".github" / "workflows" / name).read_text(encoding="utf-8")


def test_refresh_release_trigger_is_live_and_same_repo() -> None:
    workflow = read_workflow("refresh-cli-release-assets.yml")
    assert 'workflows: ["Publish Release"]' in workflow
    assert 'workflows: ["Publish Draft Release"]' not in workflow
    gate = "github.event.workflow_run.head_repository.full_name == github.repository"
    assert workflow.count(gate) >= 2


def test_rolling_desktop_uses_tauri_production_build() -> None:
    # A raw Cargo build selects Tauri's devUrl and ships a desktop that requires Vite.
    workflow = read_workflow("rolling-main-cli.yml")
    assert "npm run tauri:build -- --no-bundle" in workflow
    assert "cargo build --release --locked --manifest-path" not in workflow
    assert "Smoke packaged Windows desktop origin" in workflow
    assert "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS" in workflow
    assert "localhost:5173" in workflow
    assert "tauri\\.localhost" in workflow
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
        test_refresh_release_trigger_is_live_and_same_repo,
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
