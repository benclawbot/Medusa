from pathlib import Path

path = Path("crates/medusa-agent/src/delegation.rs")
text = path.read_text()
old = '''        let mut ambient = ModelConfig::default();
        ambient.provider = "new-default".into();
        ambient.name = "new-model".into();'''
new = '''        let ambient = ModelConfig {
            provider: "new-default".into(),
            name: "new-model".into(),
            ..Default::default()
        };'''
if old not in text:
    raise SystemExit("provider route clippy anchor missing")
path.write_text(text.replace(old, new, 1))
