from pathlib import Path

path = Path("crates/medusa-daemon/src/live_session.rs")
source = path.read_text()
old = "use medusa_runtime::attachment::{\n    AttachmentMode, ClientKind, ContinuitySession, RuntimeAttachRequest, RuntimeSessionAttachment,\n};\n"
new = "use medusa_runtime::attachment::session::{\n    AttachmentMode, ClientKind, ContinuitySession, RuntimeAttachRequest, RuntimeSessionAttachment,\n};\n"
if new not in source:
    count = source.count(old)
    if count != 1:
        raise SystemExit(f"daemon broker import target changed: {count}")
    source = source.replace(old, new, 1)

old = "        self.attachment_mut(from_client_id)?.handoff(\n"
new = "        self.attachment_mut(from_client_id)?.refresh_continuity()?;\n        self.attachment_mut(from_client_id)?.handoff(\n"
if new not in source:
    count = source.count(old)
    if count != 1:
        raise SystemExit(f"handoff refresh target changed: {count}")
    source = source.replace(old, new, 1)

path.write_text(source)
