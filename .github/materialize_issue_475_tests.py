from pathlib import Path

path = Path("crates/medusa-provider/tests/openai_provider.rs")
source = path.read_text()
old = '''                    MessageBlock::ToolResult {
                        tool_use_id: "call-0".into(),
                        content: "ok".into(),
                        is_error: false,
                    },
                    MessageBlock::Image {
                        source: ImageSource::AttachmentRef {
                            attachment_id: "img".into(),
                        },
                        alt_text: Some("ignored".into()),
                    },'''
new = '''                    MessageBlock::ToolResult {
                        tool_use_id: "call-0".into(),
                        content: "ok".into(),
                        is_error: false,
                    },'''
assert source.count(old) == 1
path.write_text(source.replace(old, new))
