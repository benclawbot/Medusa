#[test]
fn emit_materialized_telegram_runtime_source() {
    let source = include_str!("runtime.rs");
    eprintln!("MEDUSA_MATERIALIZED_RUNTIME_BEGIN");
    eprintln!("{source}");
    eprintln!("MEDUSA_MATERIALIZED_RUNTIME_END");
    panic!("materialized Telegram runtime source emitted for one-time commit");
}
