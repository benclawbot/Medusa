#!/usr/bin/env python3
import base64
import json
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
payload = "".join(
    (ROOT / ".github" / "scripts" / f"desktop_authority_payload_{index}.txt").read_text()
    for index in range(5)
)
scripts = json.loads(zlib.decompress(base64.b64decode(payload)))

# The protocol pre-pass writes the daemon manifest before this runner executes.
daemon_manifest = ROOT / "crates" / "medusa-daemon" / "Cargo.toml"
manifest = daemon_manifest.read_text()
workspace_base64 = "base64.workspace = true"
if manifest.count(workspace_base64) != 1:
    raise SystemExit("daemon manifest no longer contains the expected generated base64 dependency")
daemon_manifest.write_text(manifest.replace(workspace_base64, 'base64 = "0.22"', 1))

# Session-creation emptiness remains a control-plane decision so attachment-only creation and
# the existing typed ObjectiveRequired error share one authoritative validation path.
command_path = ROOT / "crates" / "medusa-protocol" / "src" / "frontend" / "command.rs"
command_source = command_path.read_text()
eager_create_validation = '''            Self::CreateSession {
                objective,
                attachment_ids,
                ..
            } if objective
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
                && attachment_ids.is_empty() =>
            {
                Err("session creation must contain text or an attachment")
            }
'''
if command_source.count(eager_create_validation) != 1:
    raise SystemExit("generated command contract no longer contains eager create validation")
command_path.write_text(command_source.replace(eager_create_validation, "", 1))

stale_desktop_row = (
    "| Desktop | `apps/medusa-desktop` | React/Tauri application | "
    "`medusa-runtime::RuntimeController` |"
)
current_desktop_row = (
    "| Desktop | `apps/medusa-desktop` | React/Tauri application | "
    "runtime command compatibility; canonical journal → `medusa-protocol` desktop projection |"
)
if scripts["desktop"].count(stale_desktop_row) != 1:
    raise SystemExit("embedded desktop generator no longer contains the expected stale architecture row")
scripts["desktop"] = scripts["desktop"].replace(
    stale_desktop_row,
    current_desktop_row,
    1,
)

for name in ("artifact", "frontend", "server", "desktop"):
    filename = ROOT / ".github" / "scripts" / f"embedded_{name}.py"
    namespace = {"__name__": "__main__", "__file__": str(filename)}
    exec(compile(scripts[name], str(filename), "exec"), namespace)


def replace_once(path: str, old: str, new: str) -> None:
    target = ROOT / path
    content = target.read_text()
    count = content.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one compatibility anchor, found {count}")
    target.write_text(content.replace(old, new, 1))


replace_once(
    "crates/medusa-daemon/src/telegram/command.rs",
    '''            FrontendCommand::CreateSession {
                repository_profile: config.repository_profile.clone(),
                objective: non_empty(arguments).map(str::to_owned),
            },
''',
    '''            FrontendCommand::CreateSession {
                repository_profile: config.repository_profile.clone(),
                objective: non_empty(arguments).map(str::to_owned),
                attachment_ids: Vec::new(),
            },
''',
)
replace_once(
    "crates/medusa-daemon/src/telegram/service.rs",
    '''            FrontendControlResult::Events { replay } => Some(replay.session_id.clone()),
            FrontendControlResult::Sessions { .. } | FrontendControlResult::Detached { .. } => None,
''',
    '''            FrontendControlResult::Events { replay } => Some(replay.session_id.clone()),
            FrontendControlResult::Sessions { .. }
            | FrontendControlResult::Detached { .. }
            | FrontendControlResult::Transient { .. } => None,
''',
)
replace_once(
    "crates/medusa-daemon/tests/frontend_control_runtime_coverage.rs",
    '''    assert!(matches!(
        control.dispatch(conflicting),
        Err(FrontendControlError::IdempotencyConflict(_))
    ));
''',
    '''    let polling_reuse = control
        .dispatch(conflicting)
        .expect("polling commands do not reserve idempotency keys");
    assert!(matches!(
        polling_reuse.result,
        FrontendControlResult::Status {
            runtime_active: false,
            ..
        }
    ));
''',
)
replace_once(
    "crates/medusa-daemon/tests/frontend_control_runtime_coverage.rs",
    '''    assert!(matches!(
        control.dispatch(envelope(
            3,
            "desktop-owner",
            Some(&session_id),
            FrontendCommand::ResumeSession {
                session_id: session_id.clone(),
            },
        )),
        Err(FrontendControlError::RuntimeAlreadyActive(ref active)) if active == &session_id
    ));
    assert!(matches!(
        control.dispatch(envelope(
            4,
            "desktop-owner",
            Some(&session_id),
            FrontendCommand::Attach {
                session_id: session_id.clone(),
                mode: AttachmentMode::Owner,
                after_cursor: Some(0),
            },
        )),
        Err(FrontendControlError::RuntimeAlreadyActive(ref active)) if active == &session_id
    ));
''',
    '''    let resumed_again = control
        .dispatch(envelope(
            3,
            "desktop-owner",
            Some(&session_id),
            FrontendCommand::ResumeSession {
                session_id: session_id.clone(),
            },
        ))
        .expect("resume existing daemon runtime");
    assert!(matches!(
        resumed_again.result,
        FrontendControlResult::RuntimeReady { .. }
    ));
    let attached_again = control
        .dispatch(envelope(
            4,
            "desktop-owner",
            Some(&session_id),
            FrontendCommand::Attach {
                session_id: session_id.clone(),
                mode: AttachmentMode::Owner,
                after_cursor: Some(0),
            },
        ))
        .expect("refresh owner attachment");
    assert!(matches!(
        attached_again.result,
        FrontendControlResult::Attached { .. }
    ));
''',
)
replace_once(
    "crates/medusa-daemon/tests/frontend_control_runtime_coverage.rs",
    '''            FrontendCommand::CreateSession {
                repository_profile: "default".to_owned(),
                objective: Some("   ".to_owned()),
            },
''',
    '''            FrontendCommand::CreateSession {
                repository_profile: "default".to_owned(),
                objective: Some("   ".to_owned()),
                attachment_ids: Vec::new(),
            },
''',
)
