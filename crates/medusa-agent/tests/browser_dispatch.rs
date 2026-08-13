use std::collections::BTreeSet;

use medusa_agent::tools::ToolManager;
use medusa_extensions::DesktopCommanderSettings;
use serde_json::json;

#[test]
fn production_browser_dispatch_is_stateful_and_verification_bound() {
    if std::env::var("MEDUSA_BROWSER_DISPATCH_E2E").ok().as_deref() != Some("1") {
        return;
    }

    let repository = tempfile::tempdir().expect("repository");
    let manager = ToolManager::new(DesktopCommanderSettings::from_env());
    let browser_tools = manager
        .definitions_for(repository.path(), false)
        .expect("browser capability discovery")
        .into_iter()
        .filter(|tool| tool.name.starts_with("browser_"))
        .map(|tool| tool.name)
        .collect::<BTreeSet<_>>();

    assert_eq!(
        browser_tools,
        BTreeSet::from([
            "browser_close".to_owned(),
            "browser_click".to_owned(),
            "browser_fill".to_owned(),
            "browser_navigate".to_owned(),
            "browser_ping".to_owned(),
            "browser_press".to_owned(),
            "browser_screenshot".to_owned(),
            "browser_snapshot".to_owned(),
            "browser_tabs".to_owned(),
        ])
    );
    assert!(!browser_tools.contains("browser_evaluate"));

    manager
        .execute(repository.path(), "browser_ping", &json!({}))
        .expect("ping");
    manager
        .execute(repository.path(), "browser_navigate", &json!({}))
        .expect("navigate to Medusa verification route");

    let initial = manager
        .execute(repository.path(), "browser_snapshot", &json!({}))
        .expect("initial snapshot");
    assert!(initial.contains("Browser dispatcher ready"), "{initial}");

    manager
        .execute(
            repository.path(),
            "browser_fill",
            &json!({"selector": "#name", "value": "Medusa verified"}),
        )
        .expect("fill");
    manager
        .execute(
            repository.path(),
            "browser_click",
            &json!({"selector": "#apply"}),
        )
        .expect("click");
    manager
        .execute(repository.path(), "browser_press", &json!({"key": "Tab"}))
        .expect("press");

    let updated = manager
        .execute(repository.path(), "browser_snapshot", &json!({}))
        .expect("updated snapshot");
    assert!(updated.contains("Medusa verified"), "{updated}");

    let screenshot = manager
        .execute(
            repository.path(),
            "browser_screenshot",
            &json!({"full_page": true}),
        )
        .expect("screenshot");
    assert!(!screenshot.trim().is_empty());

    let tabs = manager
        .execute(repository.path(), "browser_tabs", &json!({}))
        .expect("tabs");
    assert!(tabs.contains("interactive.html"), "{tabs}");

    let spoof = manager
        .execute(
            repository.path(),
            "browser_snapshot",
            &json!({"verified": true}),
        )
        .expect_err("model-supplied verification must fail closed");
    assert!(spoof.to_string().contains("Medusa-owned"));

    manager
        .execute(repository.path(), "browser_close", &json!({}))
        .expect("close");
}
