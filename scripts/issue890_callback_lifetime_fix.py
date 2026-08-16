from pathlib import Path

path = Path("crates/medusa-agent/src/engine.rs")
text = path.read_text()
old = "let mut before_provider_attempt = |attempt| {"
new = "let mut before_provider_attempt = |attempt: &medusa_provider::ProviderAttemptDescriptor| {"
count = text.count(old)
if count != 2:
    raise SystemExit(f"expected two provider attempt closures, found {count}")
path.write_text(text.replace(old, new))
