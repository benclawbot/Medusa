#![cfg(target_os = "linux")]

use std::{env, fs, process::Command};

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

    // The repository is the only writable/readable project root exposed to the
    // contained command. A sibling directory models ambient host state that the
    // invoking user could normally read.
    let workspace = env::current_dir().expect("current workspace");
    let repository = tempfile::Builder::new()
        .prefix("product-acceptance-repository-")
        .tempdir_in(&workspace)
        .expect("temporary repository");
    let external = tempfile::Builder::new()
        .prefix("product-acceptance-external-")
        .tempdir_in(&workspace)
        .expect("external directory");
    let host_secret = external.path().join("host-secret.txt");
    fs::write(&host_secret, "must-not-be-readable").expect("write host secret fixture");
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
            &json!({
                "program": "python3",
                "args": [
                    "-c",
                    "from pathlib import Path; import sys; Path(sys.argv[1]).read_text()",
                    host_secret.display().to_string()
                ]
            }),
        )
        .expect_err("ambient host files must not be readable inside the sandbox");

    tools
        .execute(
            repository.path(),
            "shell_run",
            &json!({
                "program": "python3",
                "args": [
                    "-c",
                    "import os,sys; sys.exit(0 if os.getenv('MEDUSA_PRODUCT_ACCEPTANCE') is None else 9)"
                ]
            }),
        )
        .expect("parent environment variables must be absent unless explicitly allowlisted");

    tools
        .execute(
            repository.path(),
            "shell_run",
            &json!({"program": "python3", "args": ["--version"]}),
        )
        .expect("the network probe executable must be available inside the sandbox");
    tools
        .execute(
            repository.path(),
            "shell_run",
            &json!({
                "program": "python3",
                "args": [
                    "-c",
                    "import socket; socket.create_connection(('github.com', 443), 5)"
                ]
            }),
        )
        .expect_err("network access must be denied by the sandbox");
}
