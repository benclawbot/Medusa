use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use medusa_agent::tools::ToolManager;
use medusa_browser_client::{BrowserClient, BrowserRequest, BrowserResponse};
use medusa_extensions::DesktopCommanderSettings;
use serde_json::json;

const E2E_ENV: &str = "MEDUSA_BROWSER_HARDENING_E2E";
const SCENARIO_ENV: &str = "MEDUSA_BROWSER_HARDENING_SCENARIO";

fn manager() -> (tempfile::TempDir, ToolManager) {
    let repository = tempfile::tempdir().expect("browser hardening repository");
    let manager = ToolManager::new(DesktopCommanderSettings::from_env());
    (repository, manager)
}

fn close(manager: &ToolManager, repository: &std::path::Path) {
    manager
        .execute(repository, "browser_close", &json!({}))
        .expect("close browser session");
}

#[test]
fn production_browser_dispatch_hardening() {
    if std::env::var(E2E_ENV).ok().as_deref() != Some("1") {
        return;
    }
    let scenario = std::env::var(SCENARIO_ENV).expect("browser hardening scenario");
    let (repository, manager) = manager();

    match scenario.as_str() {
        "timeout" => {
            manager
                .execute(repository.path(), "browser_ping", &json!({}))
                .expect("warm verified browser before deadline proof");

            let cancellation = AtomicBool::new(false);
            let started = Instant::now();
            let error = manager
                .execute_cancellable(
                    repository.path(),
                    "browser_click",
                    &json!({"selector": "#medusa-never-exists"}),
                    &cancellation,
                )
                .expect_err("pending browser request must hit its deadline");
            assert!(started.elapsed() < Duration::from_secs(12), "{error}");
            assert_eq!(
                error.context.get("browser_error_kind"),
                Some(&serde_json::json!("timeout")),
                "{error}"
            );
            assert_eq!(
                error.context.get("browser_sidecar_reset"),
                Some(&serde_json::json!(true)),
                "{error}"
            );

            manager
                .execute(repository.path(), "browser_ping", &json!({}))
                .expect("dispatcher restarts a clean sidecar after timeout");
            close(&manager, repository.path());
        }
        "cancel" => {
            manager
                .execute(repository.path(), "browser_ping", &json!({}))
                .expect("initialize browser before cancellation proof");

            let cancellation = Arc::new(AtomicBool::new(false));
            let trigger = Arc::clone(&cancellation);
            let toggler = thread::spawn(move || {
                thread::sleep(Duration::from_millis(500));
                trigger.store(true, Ordering::Release);
            });
            let started = Instant::now();
            let error = manager
                .execute_cancellable(
                    repository.path(),
                    "browser_click",
                    &json!({"selector": "#medusa-never-exists"}),
                    cancellation.as_ref(),
                )
                .expect_err("in-flight browser request must be cancellable");
            toggler.join().expect("cancellation toggler");
            assert!(started.elapsed() < Duration::from_secs(3), "{error}");
            assert_eq!(
                error.context.get("browser_error_kind"),
                Some(&serde_json::json!("cancelled")),
                "{error}"
            );
            assert_eq!(
                error.context.get("browser_sidecar_reset"),
                Some(&serde_json::json!(true)),
                "{error}"
            );

            cancellation.store(false, Ordering::Release);
            manager
                .execute_cancellable(
                    repository.path(),
                    "browser_ping",
                    &json!({}),
                    cancellation.as_ref(),
                )
                .expect("dispatcher restarts a clean sidecar after cancellation");
            manager
                .execute_cancellable(
                    repository.path(),
                    "browser_close",
                    &json!({}),
                    cancellation.as_ref(),
                )
                .expect("close restarted sidecar");
        }
        "dom" => {
            let error = manager
                .execute(repository.path(), "browser_snapshot", &json!({}))
                .expect_err("oversized DOM must fail closed inside the bridge");
            assert!(error.to_string().contains("dom_too_large"), "{error}");
            close(&manager, repository.path());
        }
        "text" => {
            let error = manager
                .execute(repository.path(), "browser_snapshot", &json!({}))
                .expect_err("oversized snapshot text must fail closed inside the bridge");
            assert!(
                error.to_string().contains("snapshot_text_too_large"),
                "{error}"
            );
            close(&manager, repository.path());
        }
        "screenshot" => {
            let error = manager
                .execute(
                    repository.path(),
                    "browser_screenshot",
                    &json!({"full_page": true}),
                )
                .expect_err("oversized screenshot dimensions must fail closed");
            assert!(
                error
                    .to_string()
                    .contains("screenshot_dimensions_too_large"),
                "{error}"
            );
            close(&manager, repository.path());
        }
        "evaluate" => {
            let browserd = std::env::var("MEDUSA_BROWSER_PATH").expect("browser sidecar path");
            let route = std::env::var("MEDUSA_BROWSER_VERIFY_URL").expect("verification route");
            let mut client = BrowserClient::spawn_with_env(
                &browserd,
                &[("MEDUSA_BROWSER_VERIFICATION_ORIGIN", route.as_str())],
            )
            .expect("spawn browser sidecar");
            let cancellation = AtomicBool::new(false);
            client
                .request_with_control(
                    BrowserRequest::Navigate { url: route },
                    Duration::from_secs(10),
                    &cancellation,
                )
                .expect("navigate before evaluation limit proof");
            let response = client
                .request_with_control(
                    BrowserRequest::Evaluate {
                        expression: "'x'.repeat(1048577)".to_owned(),
                    },
                    Duration::from_secs(10),
                    &cancellation,
                )
                .expect("bounded evaluation response");
            assert!(
                matches!(
                    response,
                    BrowserResponse::Error { ref code, .. } if code == "evaluation_too_large"
                ),
                "{response:?}"
            );
            client
                .request_with_control(
                    BrowserRequest::Close,
                    Duration::from_secs(10),
                    &cancellation,
                )
                .expect("close evaluation sidecar");
        }
        "reuse" => {
            for _ in 0..64 {
                manager
                    .execute(repository.path(), "browser_ping", &json!({}))
                    .expect("repeated browser ping");
            }
            let tabs = manager
                .execute(repository.path(), "browser_tabs", &json!({}))
                .expect("stateful browser session remains usable");
            assert!(tabs.contains("interactive"), "{tabs}");
            close(&manager, repository.path());
        }
        other => panic!("unknown browser hardening scenario: {other}"),
    }
}
