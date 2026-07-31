from pathlib import Path

path = Path("crates/medusa-daemon/src/frontend_control.rs")
source = path.read_text()
helper = '''fn attachment_mode(mode: FrontendAttachmentMode) -> AttachmentMode {
    match mode {
        FrontendAttachmentMode::Owner => AttachmentMode::Owner,
        FrontendAttachmentMode::ReadOnly => AttachmentMode::ReadOnly,
    }
}

'''
count = source.count(helper)
if count not in (0, 1):
    raise SystemExit(f"attachment helper count changed: {count}")
source = source.replace(helper, "", 1)
path.write_text(source)
