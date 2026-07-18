from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    if new in text:
        return
    if text.count(old) != 1:
        raise SystemExit(f'{path}: expected one match, found {text.count(old)}')
    file.write_text(text.replace(old, new, 1))


replace_once(
    'crates/medusa-extensions/src/desktop_commander.rs',
    '''        let mut settings = Self::default();
        settings.enabled = env_flag("MEDUSA_DESKTOP_COMMANDER_ENABLED");
''',
    '''        let mut settings = Self {
            enabled: env_flag("MEDUSA_DESKTOP_COMMANDER_ENABLED"),
            ..Self::default()
        };
''',
)

replace_once(
    'crates/medusa-extensions/src/desktop_commander.rs',
    '''        let mut settings = DesktopCommanderSettings::default();
        settings.enabled = true;
''',
    '''        let mut settings = DesktopCommanderSettings {
            enabled: true,
            ..DesktopCommanderSettings::default()
        };
''',
)

replace_once(
    'crates/medusa-agent/src/engine_support.rs',
    '''            let tools = available_tools(mode)
''',
    '''            let tools = available_tools(mode, &DesktopCommanderSettings::default())
''',
)
