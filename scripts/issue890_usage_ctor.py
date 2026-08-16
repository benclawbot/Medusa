from pathlib import Path
p = Path('crates/medusa-agent/src/usage.rs')
s = p.read_text()
old = '''            EventPayload::ModelResponseReceived {\n                response_id: Some("fixture".to_owned()),\n                usage: serde_json::to_value(usage).expect("usage json"),\n            },'''
new = '''            EventPayload::ModelResponseReceived {\n                response_id: Some("fixture".to_owned()),\n                usage: serde_json::to_value(usage).expect("usage json"),\n                request_id: None,\n                request_fingerprint: None,\n            },'''
if old not in s:
    raise SystemExit('expected usage fixture not found')
p.write_text(s.replace(old, new, 1))
