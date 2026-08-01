#[test]
fn emit_materialized_telegram_sources() {
    for (path, source) in [
        ("src/telegram/runtime.rs", include_str!("runtime.rs")),
        ("src/telegram/mod.rs", include_str!("mod.rs")),
        ("src/telegram/mini_app.rs", include_str!("mini_app.rs")),
        ("src/telegram/voice.rs", include_str!("voice.rs")),
        ("src/telegram/webhook.rs", include_str!("webhook.rs")),
        (
            "src/telegram/mini_app_http.rs",
            include_str!("mini_app_http.rs"),
        ),
        ("src/telegram/supervisor.rs", include_str!("supervisor.rs")),
        ("src/telegram/service.rs", include_str!("service.rs")),
        ("src/telegram/delivery.rs", include_str!("delivery.rs")),
        (
            "src/telegram/bot_api/mod.rs",
            include_str!("bot_api/mod.rs"),
        ),
        (
            "src/telegram/bot_api/operations.rs",
            include_str!("bot_api/operations.rs"),
        ),
        ("src/artifact_store.rs", include_str!("../artifact_store.rs")),
        (
            "src/frontend_control.rs",
            include_str!("../frontend_control.rs"),
        ),
    ] {
        eprintln!("MEDUSA_MATERIALIZED_FILE_BEGIN:{path}");
        eprintln!("{source}");
        eprintln!("MEDUSA_MATERIALIZED_FILE_END:{path}");
    }
    panic!("materialized Telegram sources emitted for one-time commit");
}
