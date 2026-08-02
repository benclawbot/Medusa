use std::process::Command;

use medusa_config::MedusaConfig;

const CHILD_MARKER: &str = "MEDUSA_CONFIG_STRUCT_CHILD";
const CHILD_SUCCESS: &str = "MEDUSA_CONFIG_STRUCT_CHILD_OK";
const CONFIG_KEYS: &[&str] = &[
    "MEDUSA_BROWSER_ENABLED",
    "MEDUSA_BROWSER_PATH",
    "MEDUSA_BROWSER_TIMEOUT_MS",
    "MEDUSA_ENVELOPE_HEAD_BYTES",
    "MEDUSA_ENVELOPE_TAIL_BYTES",
    "MEDUSA_DAEMON_MAX_ARTIFACT_BYTES",
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
    let output = command.output().expect("run isolated configuration test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains(CHILD_SUCCESS),
        "isolated test {test} failed or did not execute its assertions\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );
}

#[test]
fn from_env_reads_all_knobs() {
    run_child(
        "from_env_reads_all_knobs_child",
        &[
            ("MEDUSA_BROWSER_ENABLED", "true"),
            ("MEDUSA_BROWSER_PATH", "/opt/medusa-browserd"),
            ("MEDUSA_BROWSER_TIMEOUT_MS", "12000"),
            ("MEDUSA_ENVELOPE_HEAD_BYTES", "1024"),
            ("MEDUSA_ENVELOPE_TAIL_BYTES", "2048"),
            ("MEDUSA_DAEMON_MAX_ARTIFACT_BYTES", "1048576"),
        ],
        &[],
    );
}

#[test]
fn from_env_reads_all_knobs_child() {
    if std::env::var_os(CHILD_MARKER).is_none() {
        return;
    }
    let cfg = MedusaConfig::from_env();
    assert!(cfg.browser.enabled);
    assert_eq!(
        cfg.browser.path.as_deref(),
        Some(std::path::Path::new("/opt/medusa-browserd"))
    );
    assert_eq!(cfg.browser.timeout_ms, 12_000);
    assert_eq!(cfg.envelope.head_bytes, 1_024);
    assert_eq!(cfg.envelope.tail_bytes, 2_048);
    assert_eq!(cfg.daemon_max_artifact_bytes, 1_048_576);
    println!("{CHILD_SUCCESS}");
}

#[test]
fn from_env_uses_sensible_defaults() {
    run_child("from_env_uses_sensible_defaults_child", &[], CONFIG_KEYS);
}

#[test]
fn from_env_uses_sensible_defaults_child() {
    if std::env::var_os(CHILD_MARKER).is_none() {
        return;
    }
    let cfg = MedusaConfig::from_env();
    assert!(!cfg.browser.enabled);
    assert!(cfg.browser.path.is_none());
    assert_eq!(cfg.browser.timeout_ms, 30_000);
    assert_eq!(cfg.envelope.head_bytes, 4_096);
    assert_eq!(cfg.envelope.tail_bytes, 4_096);
    assert_eq!(cfg.daemon_max_artifact_bytes, 256 * 1024 * 1024);
    println!("{CHILD_SUCCESS}");
}
