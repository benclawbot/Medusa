#!/usr/bin/env python3
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def replace(path: str, old: str, new: str) -> None:
    target = ROOT / path
    text = target.read_text(encoding="utf-8")
    if old not in text:
        raise SystemExit(f"missing expected pattern in {path}: {old[:120]!r}")
    target.write_text(text.replace(old, new, 1), encoding="utf-8")


runtime = ROOT / "crates/medusa-runtime/src/lib.rs"
runtime_text = runtime.read_text(encoding="utf-8")
if "fn persisted_permission_mode(" not in runtime_text:
    replace(
        "crates/medusa-runtime/src/lib.rs",
        "use medusa_config::{Config, ConfigurationChanged, Mode, ProviderProfileStore};",
        "use medusa_config::{\n    Config, ConfigurationChanged, Mode, PermissionMode, PermissionStore, ProviderProfileStore,\n};",
    )
    replace(
        "crates/medusa-runtime/src/lib.rs",
        "    fn from_config_with_runtime(repo: PathBuf, mut config: Config) -> Result<Self, RuntimeError> {\n        let effective = runtime_config_effective_for_repo(&repo, &config)?;",
        "    fn from_config_with_runtime(repo: PathBuf, mut config: Config) -> Result<Self, RuntimeError> {\n        let permission_mode = PermissionStore::user()\n            .and_then(|store| store.load())\n            .map_err(RuntimeError::agent)?;\n        config.agent.mode = permission_mode.execution_mode();\n        let effective = runtime_config_effective_for_repo(&repo, &config)?;",
    )
    replace(
        "crates/medusa-runtime/src/lib.rs",
        "fn oauth_sandbox_policy(mode: Mode) -> (&'static str, &'static str) {\n    match mode {\n        Mode::ReadOnly => (\"never\", \"readOnly\"),\n        Mode::Review | Mode::Yolo => (\"on-request\", \"workspaceWrite\"),\n    }\n}\n\n#[cfg(test)]\nmod oauth_sandbox_contract_tests {\n    use super::{Mode, oauth_sandbox_policy};\n\n    #[test]\n    fn uses_codex_app_server_sandbox_variants_for_thread_and_turn() {\n        assert_eq!(oauth_sandbox_policy(Mode::ReadOnly), (\"never\", \"readOnly\"));\n        assert_eq!(\n            oauth_sandbox_policy(Mode::Review),\n            (\"on-request\", \"workspaceWrite\")\n        );\n        assert_eq!(\n            oauth_sandbox_policy(Mode::Yolo),\n            (\"on-request\", \"workspaceWrite\")\n        );\n    }\n}",
        "fn persisted_permission_mode(fallback: Mode) -> PermissionMode {\n    PermissionStore::user()\n        .and_then(|store| store.load())\n        .unwrap_or(match fallback {\n            Mode::Yolo => PermissionMode::FullAccess,\n            Mode::Review => PermissionMode::AskForApproval,\n            Mode::ReadOnly => PermissionMode::ReadOnly,\n        })\n}\n\nfn oauth_sandbox_policy(mode: PermissionMode) -> (&'static str, &'static str) {\n    match mode {\n        PermissionMode::FullAccess => (\"never\", \"dangerFullAccess\"),\n        PermissionMode::AskForApproval | PermissionMode::ApproveForMe => {\n            (\"on-request\", \"workspaceWrite\")\n        }\n        PermissionMode::ReadOnly => (\"on-request\", \"readOnly\"),\n    }\n}\n\nfn should_auto_approve_oauth(mode: PermissionMode, method: &str) -> bool {\n    match mode {\n        PermissionMode::FullAccess => matches!(\n            method,\n            \"item/commandExecution/requestApproval\"\n                | \"item/fileChange/requestApproval\"\n                | \"item/permissions/requestApproval\"\n        ),\n        PermissionMode::ApproveForMe => matches!(\n            method,\n            \"item/commandExecution/requestApproval\" | \"item/fileChange/requestApproval\"\n        ),\n        PermissionMode::AskForApproval | PermissionMode::ReadOnly => false,\n    }\n}\n\n#[cfg(test)]\nmod oauth_sandbox_contract_tests {\n    use super::{PermissionMode, oauth_sandbox_policy, should_auto_approve_oauth};\n\n    #[test]\n    fn uses_codex_app_server_permission_profiles_for_thread_and_turn() {\n        assert_eq!(\n            oauth_sandbox_policy(PermissionMode::FullAccess),\n            (\"never\", \"dangerFullAccess\")\n        );\n        assert_eq!(\n            oauth_sandbox_policy(PermissionMode::AskForApproval),\n            (\"on-request\", \"workspaceWrite\")\n        );\n        assert_eq!(\n            oauth_sandbox_policy(PermissionMode::ApproveForMe),\n            (\"on-request\", \"workspaceWrite\")\n        );\n        assert_eq!(\n            oauth_sandbox_policy(PermissionMode::ReadOnly),\n            (\"on-request\", \"readOnly\")\n        );\n        assert!(should_auto_approve_oauth(\n            PermissionMode::ApproveForMe,\n            \"item/fileChange/requestApproval\"\n        ));\n        assert!(!should_auto_approve_oauth(\n            PermissionMode::ApproveForMe,\n            \"item/permissions/requestApproval\"\n        ));\n    }\n}",
    )
    replace(
        "crates/medusa-runtime/src/lib.rs",
        "            let (approval_policy, sandbox) = oauth_sandbox_policy(config.agent.mode);",
        "            let permission_mode = persisted_permission_mode(config.agent.mode);\n            let (approval_policy, sandbox) = oauth_sandbox_policy(permission_mode);",
    )
    replace(
        "crates/medusa-runtime/src/lib.rs",
        "            openai_oauth::CodexTurnEvent::Approval(pending) => {\n                let question_result = (|| -> Result<AgentQuestion, RuntimeError> {",
        "            openai_oauth::CodexTurnEvent::Approval(pending) => {\n                let permission_mode = persisted_permission_mode(config.agent.mode);\n                if should_auto_approve_oauth(permission_mode, &pending.method) {\n                    server\n                        .respond_pending_with_answer(\"approve\")\n                        .map_err(RuntimeError::agent)?;\n                    continue;\n                }\n                let question_result = (|| -> Result<AgentQuestion, RuntimeError> {",
    )

# Repair a baseline denied clippy lint in the same runtime file so authoritative workspace lint can run.
runtime_text = runtime.read_text(encoding="utf-8")
if '.expect("session initialized")' in runtime_text:
    replace(
        "crates/medusa-runtime/src/lib.rs",
        '        let session = state.session.as_mut().expect("session initialized");',
        '        let session = state.session.as_mut().ok_or_else(|| {\n            RuntimeError::InvalidCommand("session state disappeared after initialization".to_owned())\n        })?;',
    )

# TUI shares the same persisted source of truth, shows the active mode next to the composer,
# and uses Shift+Tab (BackTab) to cycle modes while idle. Switching restarts the runtime so the
# complete persisted permission context is applied together before the next submission.
replace(
    "crates/medusa-tui/src/lib.rs",
    "use medusa_config::Config;",
    "use medusa_config::{Config, PermissionMode, PermissionStore};",
)

render = ROOT / "crates/medusa-tui/src/render.rs"
render_text = render.read_text(encoding="utf-8")
if "permission: String," not in render_text:
    replace(
        "crates/medusa-tui/src/render.rs",
        "pub(super) struct UiIdentity {\n    project: String,\n    model: String,\n    effort: String,\n}",
        "pub(super) struct UiIdentity {\n    project: String,\n    model: String,\n    effort: String,\n    permission: String,\n}",
    )
    replace(
        "crates/medusa-tui/src/render.rs",
        "            model: config.model.name,\n            effort: effort_label(config.agent.max_turns).to_owned(),\n        }\n    }\n}",
        "            model: config.model.name,\n            effort: effort_label(config.agent.max_turns).to_owned(),\n            permission: PermissionStore::user()\n                .and_then(|store| store.load())\n                .map_or_else(|_| PermissionMode::FullAccess.label().to_owned(), |mode| mode.label().to_owned()),\n        }\n    }\n\n    pub(super) fn set_permission(&mut self, mode: PermissionMode) {\n        self.permission = mode.label().to_owned();\n    }\n}",
    )
    replace(
        "crates/medusa-tui/src/render.rs",
        "    } else {\n        composer_prompt_text(&app.composer.draft.text)\n    };\n    set_frame_line(",
        "    } else {\n        composer_prompt_text(&app.composer.draft.text)\n    };\n    let prompt = format!(\"{prompt}  [{}]\", identity.permission);\n    set_frame_line(",
    )

session = ROOT / "crates/medusa-tui/src/session.rs"
session_text = session.read_text(encoding="utf-8")
if "handle_permission_mode_shortcut" not in session_text:
    replace(
        "crates/medusa-tui/src/session.rs",
        "    let identity = UiIdentity::for_repo(&options.repo);",
        "    let mut identity = UiIdentity::for_repo(&options.repo);",
    )
    replace(
        "crates/medusa-tui/src/session.rs",
        "        &identity,\n        &mut app,",
        "        &mut identity,\n        &mut app,",
    )
    replace(
        "crates/medusa-tui/src/session.rs",
        "    identity: &UiIdentity,\n    app: &mut AppState,\n    runtime: &mut RuntimeController,",
        "    identity: &mut UiIdentity,\n    app: &mut AppState,\n    runtime: &mut RuntimeController,",
    )
    # second run_loop (portable)
    replace(
        "crates/medusa-tui/src/session.rs",
        "    identity: &UiIdentity,\n    app: &mut AppState,\n    runtime: &mut RuntimeController,",
        "    identity: &mut UiIdentity,\n    app: &mut AppState,\n    runtime: &mut RuntimeController,",
    )
    # Insert shortcut handling in both event loops immediately after redraw handling.
    for _ in range(2):
        replace(
            "crates/medusa-tui/src/session.rs",
            "            if ctrl_l_redraw(&terminal_event) {\n                continue;\n            }",
            "            if ctrl_l_redraw(&terminal_event) {\n                continue;\n            }\n            if handle_permission_mode_shortcut(&terminal_event, identity, app, runtime)? {\n                continue;\n            }",
        )
    replace(
        "crates/medusa-tui/src/session.rs",
        "fn handle_mouse_selection(\n    app: &mut AppState,",
        "fn handle_permission_mode_shortcut(\n    terminal_event: &Event,\n    identity: &mut UiIdentity,\n    app: &mut AppState,\n    runtime: &mut RuntimeController,\n) -> io::Result<bool> {\n    let Event::Key(key) = terminal_event else {\n        return Ok(false);\n    };\n    if key.kind != KeyEventKind::Press || key.code != KeyCode::BackTab {\n        return Ok(false);\n    }\n    if app.is_running() {\n        app.status = \"finish the current turn before changing permissions\".to_owned();\n        return Ok(true);\n    }\n    let store = PermissionStore::user().map_err(app_error)?;\n    let current = store.load().map_err(app_error)?;\n    let position = PermissionMode::ALL\n        .iter()\n        .position(|mode| *mode == current)\n        .unwrap_or_default();\n    let next = PermissionMode::ALL[(position + 1) % PermissionMode::ALL.len()];\n    store.save(next).map_err(app_error)?;\n    identity.set_permission(next);\n    *runtime = RuntimeController::start(app.repository().to_path_buf());\n    app.status = format!(\"permissions: {}\", next.label());\n    Ok(true)\n}\n\nfn handle_mouse_selection(\n    app: &mut AppState,",
    )

print("permission mode wiring applied")
