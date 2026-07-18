from pathlib import Path

path = Path('crates/medusa-extensions/src/desktop_commander.rs')
text = path.read_text()
old = '''        let mut settings = Self::default();
        settings.enabled = env_flag("MEDUSA_DESKTOP_COMMANDER_ENABLED");
'''
new = '''        let mut settings = Self {
            enabled: env_flag("MEDUSA_DESKTOP_COMMANDER_ENABLED"),
            ..Self::default()
        };
'''
if new in text:
    raise SystemExit(0)
if text.count(old) != 1:
    raise SystemExit(f'expected one settings initialization, found {text.count(old)}')
path.write_text(text.replace(old, new, 1))
