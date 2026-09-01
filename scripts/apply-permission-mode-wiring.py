#!/usr/bin/env python3
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
session = ROOT / "crates/medusa-tui/src/session.rs"
text = session.read_text(encoding="utf-8")

call = """            if handle_permission_mode_shortcut(&terminal_event, identity, app, runtime)? {
                continue;
            }
"""
# The first migration inserted the native-loop shortcut twice. Keep exactly one there.
text = text.replace(call + call, call)

# The portable event loop needs the same shortcut. Insert it after its redraw guard.
if text.count("if handle_permission_mode_shortcut(&terminal_event, identity, app, runtime)?") == 1:
    redraw = """            if ctrl_l_redraw(&terminal_event) {
                continue;
            }
"""
    position = text.rfind(redraw)
    if position < 0:
        raise SystemExit("portable redraw guard not found")
    position += len(redraw)
    text = text[:position] + call + text[position:]

# PermissionStore returns MedusaError rather than AgentError; map it directly to io::Error.
text = text.replace(
    "let store = PermissionStore::user().map_err(app_error)?;",
    "let store = PermissionStore::user().map_err(|error| io::Error::other(error.to_string()))?;",
)
text = text.replace(
    "let current = store.load().map_err(app_error)?;",
    "let current = store.load().map_err(|error| io::Error::other(error.to_string()))?;",
)
text = text.replace(
    "store.save(next).map_err(app_error)?;",
    "store.save(next).map_err(|error| io::Error::other(error.to_string()))?;",
)
session.write_text(text, encoding="utf-8")

print("permission mode wiring fixups applied")
