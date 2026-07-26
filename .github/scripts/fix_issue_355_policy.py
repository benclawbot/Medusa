from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected exactly one match, found {count}")
    return text.replace(old, new, 1)


path = Path("crates/medusa-agent/src/verification.rs")
text = path.read_text()

text = replace_once(
    text,
    "    collections::BTreeSet,\n",
    "    collections::{BTreeMap, BTreeSet},\n",
    "collection imports",
)
text = replace_once(
    text,
    "use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};\n",
    "use medusa_config::Config;\nuse medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};\n",
    "config import",
)
text = replace_once(
    text,
    "    let browser_decision = browser_verification_decision(&changed_paths);\n",
    "    let browser_decision = browser_verification_decision(\n        &changed_paths,\n        browser_policy_enabled(repo),\n    );\n",
    "policy decision call",
)
text = replace_once(
    text,
    "fn browser_verification_decision(changed_paths: &[PathBuf]) -> BrowserVerificationDecision {\n",
    "fn browser_policy_enabled(repo: &Path) -> bool {\n    let project = repo.join(\".medusa/config.toml\");\n    let project = project.is_file().then_some(project);\n    Config::load_layers(\n        None,\n        project.as_deref(),\n        &BTreeMap::new(),\n        &BTreeMap::new(),\n    )\n    .map(|config| config.verification.browser_on_ui_change)\n    .unwrap_or(true)\n}\n\nfn browser_verification_decision(\n    changed_paths: &[PathBuf],\n    policy_enabled: bool,\n) -> BrowserVerificationDecision {\n",
    "policy helper and signature",
)
text = replace_once(
    text,
    "        _ if changed_paths\n            .iter()\n            .any(|path| is_effective_ui_change(path)) =>\n",
    "        _ if policy_enabled\n            && changed_paths\n                .iter()\n                .any(|path| is_effective_ui_change(path)) =>\n",
    "policy-aware auto branch",
)
text = text.replace(
    "browser_verification_decision(&[PathBuf::from(\"README.md\")])",
    "browser_verification_decision(&[PathBuf::from(\"README.md\")], false)",
)
text = text.replace(
    "browser_verification_decision(&[PathBuf::from(\"apps/web/App.tsx\")])",
    "browser_verification_decision(&[PathBuf::from(\"apps/web/App.tsx\")], true)",
)
marker = "    #[test]\n    fn manual_override_is_auditable() {\n"
policy_test = "    #[test]\n    fn disabled_policy_skips_automatic_ui_verification() {\n        assert_eq!(\n            browser_verification_decision(&[PathBuf::from(\"apps/web/App.tsx\")], false),\n            BrowserVerificationDecision::Skip\n        );\n    }\n\n"
text = replace_once(text, marker, policy_test + marker, "disabled policy test")
path.write_text(text)
print("issue 355 effective policy wiring applied")
