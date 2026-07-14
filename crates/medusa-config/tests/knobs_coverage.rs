#![allow(unsafe_code)]

use medusa_config::env::{
    browser_enabled, browser_path, browser_timeout_ms, envelope_head_bytes, envelope_tail_bytes,
};
use serial_test::serial;

#[test]
#[serial]
fn defaults_when_env_is_unset() {
    unsafe {
        std::env::remove_var("MEDUSA_BROWSER_ENABLED");
        std::env::remove_var("MEDUSA_BROWSER_PATH");
        std::env::remove_var("MEDUSA_BROWSER_TIMEOUT_MS");
        std::env::remove_var("MEDUSA_ENVELOPE_HEAD_BYTES");
        std::env::remove_var("MEDUSA_ENVELOPE_TAIL_BYTES");
    }
    assert!(!browser_enabled());
    assert_eq!(browser_timeout_ms(), 30_000);
    assert_eq!(envelope_head_bytes(), 4_096);
    assert_eq!(envelope_tail_bytes(), 4_096);
    assert!(browser_path().is_none());
}

#[test]
#[serial]
fn overrides_when_env_is_set() {
    unsafe {
        std::env::set_var("MEDUSA_BROWSER_ENABLED", "true");
        std::env::set_var("MEDUSA_BROWSER_PATH", "/opt/medusa-browserd");
        std::env::set_var("MEDUSA_BROWSER_TIMEOUT_MS", "15000");
        std::env::set_var("MEDUSA_ENVELOPE_HEAD_BYTES", "2048");
        std::env::set_var("MEDUSA_ENVELOPE_TAIL_BYTES", "4096");
    }
    assert!(browser_enabled());
    assert_eq!(
        browser_path().as_deref(),
        Some(std::path::Path::new("/opt/medusa-browserd"))
    );
    assert_eq!(browser_timeout_ms(), 15_000);
    assert_eq!(envelope_head_bytes(), 2_048);
    assert_eq!(envelope_tail_bytes(), 4_096);
    unsafe {
        std::env::remove_var("MEDUSA_BROWSER_ENABLED");
        std::env::remove_var("MEDUSA_BROWSER_PATH");
        std::env::remove_var("MEDUSA_BROWSER_TIMEOUT_MS");
        std::env::remove_var("MEDUSA_ENVELOPE_HEAD_BYTES");
        std::env::remove_var("MEDUSA_ENVELOPE_TAIL_BYTES");
    }
}