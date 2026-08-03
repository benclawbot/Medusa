from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"expected exactly one {label}, found {count}")
    return text.replace(old, new, 1)

projection_path = Path("crates/medusa-protocol/src/frontend/projection.rs")
text = projection_path.read_text()
text = replace_once(
    text,
    "    frontend: FrontendKind,\n) -> Option<FrontendEventEnvelope> {",
    "    frontend_kind: FrontendKind,\n) -> Option<FrontendEventEnvelope> {",
    "frontend kind parameter",
)
text = replace_once(
    text,
    'event_id: format!("{}:{}", event.event_id, frontend_label(frontend)),',
    'event_id: format!("{}:{}", event.event_id, frontend_label(frontend_kind)),',
    "frontend event identity",
)
text = replace_once(
    text,
    "fn lifecycle_for_state(state: medusa_protocol::SessionState) -> PresentationLifecycle {",
    "fn lifecycle_for_state(state: crate::SessionState) -> PresentationLifecycle {",
    "session state type path",
)
text = replace_once(
    text,
    "use medusa_protocol::SessionState;",
    "use crate::SessionState;",
    "session state test import",
)
projection_path.write_text(text)

cli_path = Path("crates/medusa-cli/src/main.rs")
text = cli_path.read_text()
text = replace_once(
    text,
    "let runtime = RuntimeController::start_with_config(repo, config);",
    "let runtime = RuntimeController::start_with_config(repo.clone(), config);",
    "headless runtime repository ownership",
)
text = replace_once(
    text,
    "let runtime = RuntimeController::start_resumed_with_config(repo, &session, config)",
    "let runtime = RuntimeController::start_resumed_with_config(repo.clone(), &session, config)",
    "resumed runtime repository ownership",
)
cli_path.write_text(text)
