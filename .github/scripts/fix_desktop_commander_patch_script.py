from pathlib import Path

path = Path('.github/scripts/apply_desktop_commander_integration.py')
text = path.read_text()
old = '''replace_once(
    "crates/medusa-agent/src/engine.rs",
    "fn json_error(error: serde_json::Error) -> MedusaError {",
    ''' + "'''" + '''fn audited_tool_name(name: &str, input: &serde_json::Value) -> String {
    if name == "desktop_commander" {
        if let Some(tool) = input.get("tool").and_then(serde_json::Value::as_str) {
            return format!("desktop_commander:{tool}");
        }
    }
    name.to_owned()
}

fn json_error(error: serde_json::Error) -> MedusaError {''' + "'''" + ''',
)
'''
new = '''replace_once(
    "crates/medusa-agent/src/engine.rs",
    "impl<P: ModelProvider> AgentEngine<P> {",
    ''' + "'''" + '''fn audited_tool_name(name: &str, input: &serde_json::Value) -> String {
    if name == "desktop_commander" {
        if let Some(tool) = input.get("tool").and_then(serde_json::Value::as_str) {
            return format!("desktop_commander:{tool}");
        }
    }
    name.to_owned()
}

impl<P: ModelProvider> AgentEngine<P> {''' + "'''" + ''',
)
'''
if old not in text:
    raise SystemExit('engine helper patch block not found')
path.write_text(text.replace(old, new, 1))
