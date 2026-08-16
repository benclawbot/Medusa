from pathlib import Path

path = Path("crates/medusa-protocol/src/frontend/projection.rs")
text = path.read_text()
old = "EventPayload::ModelRequestStarted { provider, model } =>"
new = "EventPayload::ModelRequestStarted { provider, model, .. } =>"
if text.count(old) != 1:
    raise SystemExit(f"expected exactly one old ModelRequestStarted projection pattern, found {text.count(old)}")
path.write_text(text.replace(old, new, 1))
