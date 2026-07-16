#![allow(unsafe_code)]

use medusa_config::MedusaConfig;
use serial_test::serial;

#[test]
#[serial]
fn from_env_reads_all_knobs() {
    unsafe {
        std::env::set_var("MEDUSA_BROWSER_ENABLED", "true");
        std::env::set_var("MEDUSA_BROWSER_PATH", "/opt/medusa-browserd");
        std::env::set_var("MEDUSA_BROWSER_TIMEOUT_MS", "12000");
        std::env::set_var("MEDUSA_ENVELOPE_HEAD_BYTES", "1024");
        std::env::set_var("MEDUSA_ENVELOPE_TAIL_BYTES", "2048");
        std::env::set_var("MEDUSA_DAEMON_MAX_ARTIFACT_BYTES", "1048576");
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
    unsafe {
        std::env::remove_var("MEDUSA_BROWSER_ENABLED");
        std::env::remove_var("MEDUSA_BROWSER_PATH");
        std::env::remove_var("MEDUSA_BROWSER_TIMEOUT_MS");
        std::env::remove_var("MEDUSA_ENVELOPE_HEAD_BYTES");
        std::env::remove_var("MEDUSA_ENVELOPE_TAIL_BYTES");
        std::env::remove_var("MEDUSA_DAEMON_MAX_ARTIFACT_BYTES");
    }
}

#[test]
#[serial]
fn from_env_uses_sensible_defaults() {
    unsafe {
        std::env::remove_var("MEDUSA_BROWSER_ENABLED");
        std::env::remove_var("MEDUSA_BROWSER_PATH");
        std::env::remove_var("MEDUSA_BROWSER_TIMEOUT_MS");
        std::env::remove_var("MEDUSA_ENVELOPE_HEAD_BYTES");
        std::env::remove_var("MEDUSA_ENVELOPE_TAIL_BYTES");
        std::env::remove_var("MEDUSA_DAEMON_MAX_ARTIFACT_BYTES");
    }
    let cfg = MedusaConfig::from_env();
    assert!(!cfg.browser.enabled);
    assert!(cfg.browser.path.is_none());
    assert_eq!(cfg.browser.timeout_ms, 30_000);
    assert_eq!(cfg.envelope.head_bytes, 4_096);
    assert_eq!(cfg.envelope.tail_bytes, 4_096);
    assert_eq!(cfg.daemon_max_artifact_bytes, 256 * 1024 * 1024);
}
