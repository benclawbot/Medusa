use std::{fs, process::Command};

fn run_health(repo: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_medusa"))
        .arg("--repo")
        .arg(repo)
        .args(args)
        .output()
        .expect("run health command")
}

#[test]
fn health_is_bounded_truthful_and_json_stable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let output = run_health(temp.path(), &["health", "--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("health json");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["safe_to_continue"], true);
    assert_eq!(report["components"][0]["id"], "analysis_workspace");
    assert_eq!(report["components"][0]["status"], "optional_unavailable");
    let behavioral_health = report["components"]
        .as_array()
        .expect("components")
        .iter()
        .find(|component| component["id"] == "behavioral_health")
        .expect("shared behavioral health component");
    assert_eq!(behavioral_health["status"], "optional_unavailable");
    assert!(
        behavioral_health["summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("InsufficientEvidence"))
    );
    assert!(report["components"].as_array().expect("components").len() <= 32);
}

#[test]
fn support_bundle_is_local_versioned_and_excludes_sensitive_classes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("support.json");
    let output = run_health(
        temp.path(),
        &[
            "health",
            "--json",
            "--support-bundle",
            path.to_str().expect("bundle path"),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("manifest json");
    assert_eq!(manifest["schema_version"], 1);
    assert!(
        manifest["bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes <= 512 * 1024)
    );
    let bundle = fs::read_to_string(path).expect("bundle");
    assert!(bundle.contains("credentials, OAuth tokens"));
    assert!(bundle.contains("hidden reasoning"));
    assert!(!bundle.contains("OPENAI_API_KEY"));
}
