#![cfg(target_os = "linux")]

use std::{env, process::Command};

use medusa_agent::tools::ToolManager;
use medusa_extensions::DesktopCommanderSettings;
use serde_json::json;

#[test]
fn linux_product_boundary_exercises_allowed_write_external_denial_and_network_denial() {
    if env::var_os("MEDUSA_PRODUCT_ACCEPTANCE").is_none() {
        return;
    }

    let bwrap = Command::new("bwrap")
        .arg("--version")
        .output()
        .expect("product acceptance requires the Bubblewrap backend");
    assert!(bwrap.status.success(), "Bubblewrap must be runnable");

    let repository = tempfile::tempdir().expect("temporary repository");
    let external = tempfile::tempdir().expect("external directory");
    let tools = ToolManager::new(DesktopCommanderSettings::default());

    tools
        .execute(
            repository.path(),
            "shell_run",
            &json!({"program": "touch", "args": ["accepted.txt"]}),
        )
        .expect("repository-bounded write must succeed inside the sandbox");
    assert!(repository.path().join("accepted.txt").is_file());

    let escaped = external.path().join("escape.txt");
    tools
        .execute(
            repository.path(),
            "shell_run",
            &json!({"program": "touch", "args": [escaped.display().to_string()]}),
        )
        .expect_err("external writes must be denied by the sandbox");
    assert!(!escaped.exists());

    tools
        .execute(
            repository.path(),
            "shell_run",
            &json!({"program": "git", "args": ["--version"]}),
        )
        .expect("the network probe executable must be available inside the sandbox");
    tools
        .execute(
            repository.path(),
            "shell_run",
            &json!({
                "program": "git",
                "args": ["ls-remote", "https://github.com/rust-lang/rust.git", "HEAD"]
            }),
        )
        .expect_err("network access must be denied by the sandbox");
}
