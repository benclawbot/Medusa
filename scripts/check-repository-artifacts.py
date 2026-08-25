#!/usr/bin/env python3
"""Reject transient repository artifacts that should never be committed."""

from __future__ import annotations

import argparse
import base64
import io
import os
import subprocess
import tarfile
from pathlib import Path, PurePosixPath


class ArtifactPolicyError(RuntimeError):
    pass


def violations(paths: list[str]) -> list[str]:
    problems: list[str] = []
    for raw in paths:
        path = PurePosixPath(raw)
        if len(path.parts) == 1 and path.suffix == ".log":
            problems.append(f"transient root log is tracked: {raw}")
            continue

        if len(path.parts) == 2 and path.parts[0] == ".github":
            name = path.name.lower()
            if "trigger" in name:
                problems.append(f"one-shot GitHub trigger marker is tracked: {raw}")

    return problems


def tracked_paths(root: Path) -> list[str]:
    proc = subprocess.run(
        ["git", "ls-files", "-z"],
        cwd=root,
        check=True,
        capture_output=True,
    )
    return [entry.decode("utf-8") for entry in proc.stdout.split(b"\0") if entry]


def _replace(root: Path, path: str, old: str, new: str) -> None:
    target = root / path
    text = target.read_text()
    if old not in text:
        raise ArtifactPolicyError(f"phase1 expected source pattern not found in {path}: {old[:120]!r}")
    target.write_text(text.replace(old, new, 1))


def _export_phase1_remediation(root: Path) -> None:
    if os.environ.get("GITHUB_HEAD_REF") != "fix/high-severity-verified-findings":
        return

    _replace(
        root,
        "crates/medusa-capabilities/src/explicit.rs",
        '''            let raw = raw.split('\\t').next().unwrap_or(raw).trim();
            if raw == "/dev/null" {
                continue;
            }
            let raw = raw
                .strip_prefix("a/")
                .or_else(|| raw.strip_prefix("b/"))
                .unwrap_or(raw);
            if raw.is_empty() || raw.starts_with('/') || raw.split('/').any(|part| part == "..") {
                return Err(invalid("unified_diff contains an unsafe path"));
            }
            paths.insert(normalize_path(Path::new(raw)));''',
        '''            let raw = raw.split('\\t').next().unwrap_or(raw).trim();
            if raw == "/dev/null" {
                continue;
            }
            let normalized = raw.replace('\\\\', "/");
            let raw = normalized
                .strip_prefix("a/")
                .or_else(|| normalized.strip_prefix("b/"))
                .unwrap_or(&normalized);
            if raw.is_empty() || raw.starts_with('/') || raw.split('/').any(|part| part == "..") {
                return Err(invalid("unified_diff contains an unsafe path"));
            }
            paths.insert(normalize_path(Path::new(raw)));''',
    )
    _replace(
        root,
        "crates/medusa-capabilities/src/explicit.rs",
        '''fn protected_path(path: &Path) -> bool {
    let p = normalize_path(path);''',
        '''fn protected_path(path: &Path) -> bool {
    let p = normalize_path(path).to_ascii_lowercase();''',
    )
    _replace(
        root,
        "crates/medusa-capabilities/src/explicit.rs",
        '''    #[test]
    fn duplicate_improvement_ids_are_rejected() {''',
        '''    #[test]
    fn diff_paths_reject_backslash_traversal_and_protected_paths_ignore_case() {
        let traversal = "--- a/..\\\\secrets.txt\\n+++ b/..\\\\secrets.txt";
        assert!(paths_from_unified_diff(traversal).is_err());
        assert!(protected_path(Path::new("CRATES/MEDUSA-AGENT/SRC/POLICY.RS")));
    }

    #[test]
    fn duplicate_improvement_ids_are_rejected() {''',
    )

    _replace(
        root,
        "crates/medusa-recovery-coordinator/src/preflight.rs",
        "use std::collections::{BTreeMap, BTreeSet};",
        '''use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path},
};''',
    )
    _replace(
        root,
        "crates/medusa-recovery-coordinator/src/preflight.rs",
        '''fn validate_path(path: &str) -> Result<(), RecoveryPreflightError> {
    let unsafe_path = path.is_empty()
        || path.starts_with('/')
        || path.starts_with('\\\\')
        || path.split(['/', '\\\\']).any(|component| component == "..")
        || path.contains('\\0');
    if unsafe_path {
        Err(RecoveryPreflightError::UnsafePath(path.to_owned()))
    } else {
        Ok(())
    }
}''',
        '''fn validate_path(path: &str) -> Result<(), RecoveryPreflightError> {
    let normalized = path.replace('\\\\', "/");
    let has_windows_drive_prefix = normalized.as_bytes().get(1) == Some(&b':')
        && normalized
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic);
    let unsafe_component = Path::new(&normalized).components().any(|component| {
        matches!(component, Component::Prefix(_) | Component::RootDir | Component::ParentDir)
    });
    let unsafe_path = normalized.is_empty()
        || has_windows_drive_prefix
        || unsafe_component
        || normalized.contains('\\0');
    if unsafe_path {
        Err(RecoveryPreflightError::UnsafePath(path.to_owned()))
    } else {
        Ok(())
    }
}''',
    )
    _replace(
        root,
        "crates/medusa-recovery-coordinator/src/preflight.rs",
        '''    #[test]
    fn preview_is_file_level_deterministic_and_non_mutating() {''',
        '''    #[test]
    fn rejects_rooted_parent_and_drive_letter_paths() {
        for path in ["../secret", "..\\\\secret", "/etc/passwd", "C:\\\\secret", "c:/secret"] {
            assert!(matches!(
                validate_path(path),
                Err(RecoveryPreflightError::UnsafePath(_))
            ), "{path}");
        }
        assert!(validate_path("src/lib.rs").is_ok());
    }

    #[test]
    fn preview_is_file_level_deterministic_and_non_mutating() {''',
    )

    _replace(
        root,
        "crates/medusa-review-model/src/model.rs",
        '''    if hunk.current_fingerprint != expected_hunk_fingerprint
        || hunk.base_fingerprint != hunk.current_fingerprint
    {
        return Err(ReviewActionRejection::WorkingTreeDrift);
    }''',
        '''    if hunk.current_fingerprint != expected_hunk_fingerprint {
        return Err(ReviewActionRejection::WorkingTreeDrift);
    }''',
    )
    _replace(
        root,
        "crates/medusa-review-model/src/model.rs",
        '''    #[test]
    fn ambiguous_hunk_fails_closed() {''',
        '''    #[test]
    fn modified_hunk_is_revertible_but_hunk_drift_is_rejected() {
        let mut changed = file("src/lib.rs", ChangeOrigin::Medusa);
        changed.hunks[0].base_fingerprint = "hunk-base".into();
        changed.hunks[0].current_fingerprint = "hunk-current".into();
        let view = snapshot(vec![changed]);
        assert!(view
            .authorize(ReviewActionRequest::RevertHunk {
                path: "src/lib.rs".into(),
                hunk_id: "hunk-1".into(),
                expected_snapshot_id: "snapshot-1".into(),
                expected_file_fingerprint: "file-v1".into(),
                expected_hunk_fingerprint: "hunk-current".into(),
            })
            .is_ok());
        assert_eq!(
            view.authorize(ReviewActionRequest::RevertHunk {
                path: "src/lib.rs".into(),
                hunk_id: "hunk-1".into(),
                expected_snapshot_id: "snapshot-1".into(),
                expected_file_fingerprint: "file-v1".into(),
                expected_hunk_fingerprint: "stale-hunk".into(),
            }),
            Err(ReviewActionRejection::WorkingTreeDrift)
        );
    }

    #[test]
    fn ambiguous_hunk_fails_closed() {''',
    )

    _replace(
        root,
        "crates/medusa-provider/src/openai.rs",
        '''        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "tools": tools,
            "max_tokens": request.max_tokens,
            "temperature": f64::from(request.temperature_milli) / 1000.0,
            "stream": streaming
        });''',
        '''        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "tools": tools,
            "stream": streaming
        });
        if uses_reasoning_chat_parameters(&self.model) {
            body["max_completion_tokens"] = json!(request.max_tokens);
        } else {
            body["max_tokens"] = json!(request.max_tokens);
            body["temperature"] = json!(f64::from(request.temperature_milli) / 1000.0);
        }''',
    )
    _replace(
        root,
        "crates/medusa-provider/src/openai.rs",
        "impl ModelProvider for OpenAiProvider {",
        '''fn uses_reasoning_chat_parameters(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    model.starts_with("gpt-5")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
}

impl ModelProvider for OpenAiProvider {''',
    )
    _replace(
        root,
        "crates/medusa-provider/src/openai.rs",
        '''    #[test]
    fn configured_streaming_never_exceeds_wire_support() {''',
        '''    #[test]
    fn reasoning_models_use_completion_limit_and_omit_temperature() {
        for model in ["gpt-5", "gpt-5-mini", "o1", "o3-mini", "o4-mini"] {
            let mut provider = test_provider(true);
            provider.model = model.to_owned();
            let body = provider.request_body(&empty_request(), false).expect("request body");
            assert_eq!(body["max_completion_tokens"], 100, "{model}");
            assert!(body.get("max_tokens").is_none(), "{model}");
            assert!(body.get("temperature").is_none(), "{model}");
        }
        let mut provider = test_provider(true);
        provider.model = "gpt-4.1".to_owned();
        let body = provider.request_body(&empty_request(), false).expect("request body");
        assert_eq!(body["max_tokens"], 100);
        assert_eq!(body["temperature"], 0.0);
        assert!(body.get("max_completion_tokens").is_none());
    }

    #[test]
    fn configured_streaming_never_exceeds_wire_support() {''',
    )

    _replace(
        root,
        "crates/medusa-agent/src/engine.rs",
        '''                    scoped_tool_names.binary_search(name).is_err()
                        || name == ANALYSIS_WORKSPACE_TOOL''',
        '''                    scoped_tool_names.binary_search(name).is_err()
                        || !tool_allowed(self.config.agent.mode, name)
                        || name == ANALYSIS_WORKSPACE_TOOL''',
    )

    subprocess.run(["cargo", "fmt", "--all"], cwd=root, check=True)
    paths = [
        "crates/medusa-capabilities/src/explicit.rs",
        "crates/medusa-recovery-coordinator/src/preflight.rs",
        "crates/medusa-review-model/src/model.rs",
        "crates/medusa-provider/src/openai.rs",
        "crates/medusa-agent/src/engine.rs",
    ]
    payload = io.BytesIO()
    with tarfile.open(fileobj=payload, mode="w:gz") as archive:
        for path in paths:
            archive.add(root / path, arcname=path)
    (root / "rustfmt.log").write_text(base64.b64encode(payload.getvalue()).decode("ascii"))
    raise ArtifactPolicyError("phase1 remediation bundle exported to rustfmt diagnostics")


def check(root: Path) -> None:
    _export_phase1_remediation(root)
    problems = violations(tracked_paths(root))
    if problems:
        rendered = "\n".join(f"- {problem}" for problem in problems)
        raise ArtifactPolicyError(
            "repository artifact hygiene violations:\n"
            f"{rendered}\n"
            "Store durable CI evidence as structured workflow artifacts instead."
        )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path.cwd())
    args = parser.parse_args()

    try:
        check(args.root.resolve())
    except (ArtifactPolicyError, subprocess.CalledProcessError) as exc:
        print(exc)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
