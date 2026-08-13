use std::{
    env, fs,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use medusa_agent::ToolManager;
use medusa_extensions::DesktopCommanderSettings;
use serial_test::serial;
use serde_json::json;

struct BrowserEnv {
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl BrowserEnv {
    fn install(repo: &Path, bridge_source: &str) -> Self {
        let bridge = repo.join("cancel-bridge.mjs");
        fs::write(&bridge, bridge_source).expect("write bridge fixture");
        let values = [
            ("MEDUSA_BROWSER_ENABLED", std::ffi::OsString::from("1")),
            (
                "MEDUSA_BROWSER_PATH",
                env::var_os("BROWSERD_PATH").expect("BROWSERD_PATH from certification"),
            ),
            (
                "MEDUSA_BROWSER_VERIFY_URL",
                std::ffi::OsString::from("https://github.com/"),
            ),
            ("MEDUSA_BROWSER_BRIDGE_PATH", bridge.into_os_string()),
            (
                "MEDUSA_BROWSER_TIMEOUT_MS",
                std::ffi::OsString::from("10000"),
            ),
        ];
        let previous = values
            .iter()
            .map(|(key, _)| (*key, env::var_os(key)))
            .collect::<Vec<_>>();
        for (key, value) in values {
            // SAFETY: the test is serialized because browser configuration is process-global.
            unsafe { env::set_var(key, value) };
        }
        Self { previous }
    }
}

impl Drop for BrowserEnv {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..) {
            match value {
                Some(value) => unsafe { env::set_var(key, value) },
                None => unsafe { env::remove_var(key) },
            }
        }
    }
}

#[test]
#[serial]
fn production_dispatcher_cancels_in_flight_browser_request_and_resets_sidecar() {
    let repo = tempfile::tempdir().expect("repo");
    let _environment = BrowserEnv::install(
        repo.path(),
        "process.stdin.resume(); setInterval(() => {}, 1000);",
    );
    let manager = ToolManager::new(DesktopCommanderSettings::default());
    let cancellation = Arc::new(AtomicBool::new(false));
    let trigger = Arc::clone(&cancellation);
    let toggler = thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        trigger.store(true, Ordering::Release);
    });

    let started = Instant::now();
    let error = manager
        .execute_cancellable(
            repo.path(),
            "browser_ping",
            &json!({}),
            cancellation.as_ref(),
        )
        .expect_err("in-flight browser request must be cancellable");
    toggler.join().expect("cancellation toggler");

    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(
        error.context.get("browser_error_kind"),
        Some(&serde_json::json!("cancelled"))
    );
    assert_eq!(
        error.context.get("browser_sidecar_reset"),
        Some(&serde_json::json!(true))
    );
}
