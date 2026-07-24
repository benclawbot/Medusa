from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"expected one match in {path}, found {count}: {old[:120]!r}")
    file.write_text(text.replace(old, new, 1))


replace_once(
    "crates/medusa-runtime/src/support.rs",
    "use super::{RuntimeActivity, RuntimeActivityKind, RuntimeError, RuntimeEvent, RuntimeState};\n",
    "use super::{\n    RuntimeActivity, RuntimeActivityKind, RuntimeError, RuntimeEvent, RuntimeState, TurnUsage,\n};\n",
)

replace_once(
    "crates/medusa-tui/src/lib.rs",
    "    #[test]\n    fn context_meter_shows_current_window_use_and_progress() {\n",
    "    #[test]\n    fn authoritative_usage_renders_cost_rate_and_provider_provenance() {\n        let directory = tempfile::tempdir().expect(\"tempdir\");\n        let mut app = AppState::new(\n            directory.path().to_path_buf(),\n            \"authoritative-usage\",\n            \"\",\n            Arc::new(UnsupportedClipboard),\n        )\n        .expect(\"app\");\n        app.record_turn_usage(\n            1_000,\n            500,\n            100,\n            50,\n            1_650,\n            2_000,\n            825_000,\n            12_345,\n            \"provider\".to_owned(),\n        );\n        assert_eq!(\n            session_metrics_line(&app),\n            \"session 0s · total 1.6k · input 1.0k · output 500 · cache-read 100 · cache-write 50 · cost $0.0123 · provider · 825.0 tok/s\"\n        );\n    }\n\n    #[test]\n    fn context_meter_shows_current_window_use_and_progress() {\n",
)

Path("scripts/apply-frontend-telemetry-final.py").unlink()
Path(".github/workflows/apply-frontend-telemetry-final.yml").unlink()
