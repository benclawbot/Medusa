from pathlib import Path
import re

path = Path("crates/medusa-config/src/lib.rs")
text = path.read_text()
new = '''    let mut model = toml::map::Map::new();
    model.insert("provider".to_owned(), toml::Value::String(profile.provider));
    model.insert("name".to_owned(), toml::Value::String(profile.model));
    model.insert("protocol".to_owned(), toml::Value::String(protocol.to_owned()));
    model.insert("auth".to_owned(), toml::Value::String(profile.auth));
    model.insert("speed".to_owned(), toml::Value::String(profile.speed));
    model.insert("reasoning".to_owned(), toml::Value::String(profile.reasoning));
    let mut root = toml::map::Map::new();
    root.insert("model".to_owned(), toml::Value::Table(model));
    let overlay = toml::Value::Table(root);
'''
if new not in text:
    pattern = re.compile(
        r"\s*let overlay = toml::Value::try_from\(toml::toml! \{.*?\}\)"
        r"\.map_err\(\|error\| invalid\(error\.to_string\(\)\)\)\?;\n",
        re.DOTALL,
    )
    text, count = pattern.subn("\n" + new, text, count=1)
    if count != 1:
        raise SystemExit("provider overlay marker not found")
path.write_text(text)
