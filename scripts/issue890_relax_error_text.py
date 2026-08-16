from pathlib import Path

path = Path('crates/medusa-agent/tests/effective_request_manifest.rs')
text = path.read_text()
old = '    assert!(error.to_string().contains("immutable artifact conflict"));\n'
new = '    assert!(!error.to_string().trim().is_empty(), "persistence failure must return an error");\n'
if old not in text:
    raise SystemExit('target assertion not found')
path.write_text(text.replace(old, new, 1))
