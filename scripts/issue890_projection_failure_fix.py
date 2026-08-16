from pathlib import Path

path = Path("crates/medusa-protocol/src/frontend/projection.rs")
text = path.read_text()
old = '''        EventPayload::ModelResponseReceived { usage, .. } => {\n            let input_tokens = integer(usage, &["input_tokens", "inputTokens"]);\n'''
new = '''        EventPayload::ModelRequestFailed { .. } => FrontendEvent::Activity(activity(\n            event,\n            PresentationActivityKind::Assistant,\n            PresentationLifecycle::Failed,\n            "Model request failed".to_owned(),\n            Vec::new(),\n            None,\n        )),\n        EventPayload::ModelResponseReceived { usage, .. } => {\n            let input_tokens = integer(usage, &["input_tokens", "inputTokens"]);\n'''
if text.count(old) != 1:
    raise SystemExit(f"expected one ModelResponseReceived projection anchor, found {text.count(old)}")
path.write_text(text.replace(old, new, 1))
