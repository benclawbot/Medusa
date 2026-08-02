use std::process::Command;

use medusa_config::env::{
    browser_enabled, browser_path, browser_timeout_ms, envelope_head_bytes, envelope_tail_bytes,
};

const CHILD_MARKER: &str = "MEDUSA_KNOBS_COVERAGE_CHILD";
const CHILD_SUCCESS: &str = "MEDUSA_KNOBS_COVERAGE_CHILD_OK";
const CONFIG_KEYS: &[&str] = &[
    "MEDUSA_BROWSER_ENABLED",
    "MEDUSA_BROWSER_PATH",
    "MEDUSA_BROWSER_TIMEOUT_MS",
    "MEDUSA_ENVELOPE_HEAD_BYTES",
    "MEDUSA_ENVELOPE_TAIL_BYTES",
];

fn run_child(test: &str, values: &[(&str, &str)], removed: &[&str]) {
    let mut command = Command::new(std::env::current_exe().expect("current test executable"));
    command
        .args(["--exact", test, "--nocapture"])
        .env(CHILD_MARKER, "1");
    for key in removed {
        command.env_remove(*key);
    }
    for &(key, value) in values {
        command.env(key, value);
    }
    let output = command.output().expect("run isolated environment test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains(CHILD_SUCCESS),
        "isolated test {test} failed or did not execute its assertions\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );
}

#[test]
fn defaults_when_env_is_unset() {
    run_child("defaults_when_env_is_unset_child", &[], CONFIG_KEYS);
}

#[test]
fn defaults_when_env_is_unset_child() {
    if std::env::var_os(CHILD_MARKER).is_none() {
        return;
    }
    assert!(!browser_enabled());
    assert_eq!(browser_timeout_ms(), 30_000);
    assert_eq!(envelope_head_bytes(), 4_096);
    assert_eq!(envelope_tail_bytes(), 4_096);
    assert!(browser_path().is_none());
    println!("{CHILD_SUCCESS}");
}

#[test]
fn overrides_when_env_is_set() {
    run_child(
        "overrides_when_env_is_set_child",
        &[
            ("MEDUSA_BROWSER_ENABLED", "true"),
            ("MEDUSA_BROWSER_PATH", "/opt/medusa-browserd"),
            ("MEDUSA_BROWSER_TIMEOUT_MS", "15000"),
            ("MEDUSA_ENVELOPE_HEAD_BYTES", "2048"),
            ("MEDUSA_ENVELOPE_TAIL_BYTES", "4096"),
        ],
        &[],
    );
}

#[test]
fn overrides_when_env_is_set_child() {
    if std::env::var_os(CHILD_MARKER).is_none() {
        return;
    }
    assert!(browser_enabled());
    assert_eq!(
        browser_path().as_deref(),
        Some(std::path::Path::new("/opt/medusa-browserd"))
    );
    assert_eq!(browser_timeout_ms(), 15_000);
    assert_eq!(envelope_head_bytes(), 2_048);
    assert_eq!(envelope_tail_bytes(), 4_096);
    println!("{CHILD_SUCCESS}");
}
