#!/usr/bin/env python3
import subprocess
from pathlib import Path


def rep(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    if old not in text:
        raise SystemExit(f"expected source pattern missing in {path}: {old[:120]!r}")
    p.write_text(text.replace(old, new, 1))


rep(
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
rep(
    "crates/medusa-capabilities/src/explicit.rs",
    'fn protected_path(path: &Path) -> bool {\n    let p = normalize_path(path);',
    'fn protected_path(path: &Path) -> bool {\n    let p = normalize_path(path).to_ascii_lowercase();',
)
rep(
    "crates/medusa-capabilities/src/explicit.rs",
    '    #[test]\n    fn duplicate_improvement_ids_are_rejected() {',
    '''    #[test]
    fn diff_paths_reject_backslash_traversal_and_protected_paths_ignore_case() {
        let traversal = "--- a/..\\\\secrets.txt\\n+++ b/..\\\\secrets.txt";
        assert!(paths_from_unified_diff(traversal).is_err());
        assert!(protected_path(Path::new("CRATES/MEDUSA-AGENT/SRC/POLICY.RS")));
    }

    #[test]
    fn duplicate_improvement_ids_are_rejected() {''',
)

rep(
    "crates/medusa-recovery-coordinator/src/preflight.rs",
    'use std::collections::{BTreeMap, BTreeSet};',
    '''use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path},
};''',
)
rep(
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
rep(
    "crates/medusa-recovery-coordinator/src/preflight.rs",
    '    #[test]\n    fn preview_is_file_level_deterministic_and_non_mutating() {',
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

rep(
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
rep(
    "crates/medusa-review-model/src/model.rs",
    '    #[test]\n    fn ambiguous_hunk_fails_closed() {',
    '''    #[test]
    fn modified_hunk_is_revertible_but_hunk_drift_is_rejected() {
        let mut changed = file("src/lib.rs", ChangeOrigin::Medusa);
        changed.hunks[0].base_fingerprint = "hunk-base".into();
        changed.hunks[0].current_fingerprint = "hunk-current".into();
        let view = snapshot(vec![changed]);
        assert!(view.authorize(ReviewActionRequest::RevertHunk {
            path: "src/lib.rs".into(),
            hunk_id: "hunk-1".into(),
            expected_snapshot_id: "snapshot-1".into(),
            expected_file_fingerprint: "file-v1".into(),
            expected_hunk_fingerprint: "hunk-current".into(),
        }).is_ok());
        assert_eq!(view.authorize(ReviewActionRequest::RevertHunk {
            path: "src/lib.rs".into(),
            hunk_id: "hunk-1".into(),
            expected_snapshot_id: "snapshot-1".into(),
            expected_file_fingerprint: "file-v1".into(),
            expected_hunk_fingerprint: "stale-hunk".into(),
        }), Err(ReviewActionRejection::WorkingTreeDrift));
    }

    #[test]
    fn ambiguous_hunk_fails_closed() {''',
)

rep(
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
rep(
    "crates/medusa-provider/src/openai.rs",
    'impl ModelProvider for OpenAiProvider {',
    '''fn uses_reasoning_chat_parameters(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    model.starts_with("gpt-5")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
}

impl ModelProvider for OpenAiProvider {''',
)
rep(
    "crates/medusa-provider/src/openai.rs",
    '    #[test]\n    fn configured_streaming_never_exceeds_wire_support() {',
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

rep(
    "crates/medusa-runtime/src/lib.rs",
    '''#[cfg(test)]
fn should_capture_review_baseline(general_chat: bool, resuming_pending_question: bool) -> bool {
    !general_chat && !resuming_pending_question
}

''',
    "",
)

subprocess.run(["cargo", "fmt", "--all"], check=True)

# The product commit is allowed only after a complete workspace test. The hook
# lives under .git, so it is never committed and disappears with the CI runner.
hook = Path(".git/hooks/pre-commit")
hook.write_text(
    "#!/usr/bin/env bash\n"
    "set -euo pipefail\n"
    "MEDUSA_ALLOW_INSECURE_PROVIDER_HTTP=1 cargo test --workspace --all-features --locked\n"
    "git reset -q HEAD -- .github/workflows/ci.yml scripts/check-repository-artifacts.py "
    ".github/workflows/phase1-high-severity-fix.yml .github/workflows/phase1-pr-remediation.yml "
    "scripts/apply-phase1-remediation.py\n"
)
hook.chmod(0o755)
