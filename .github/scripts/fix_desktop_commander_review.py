from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    if new in text:
        return
    if text.count(old) != 1:
        raise SystemExit(f"{path}: expected one anchor, found {text.count(old)}")
    file.write_text(text.replace(old, new, 1))


replace_once(
    "crates/medusa-extensions/src/desktop_commander.rs",
    '''    #[must_use]
    pub fn effective_tools(&self) -> BTreeSet<String> {
        self.allowed_tools
            .iter()
            .filter(|tool| self.tool_allowed(tool, false))
            .cloned()
            .collect()
    }
''',
    '''    #[must_use]
    pub fn effective_tools(&self) -> BTreeSet<String> {
        self.effective_tools_for_mode(false)
    }

    #[must_use]
    pub fn effective_tools_for_mode(&self, read_only: bool) -> BTreeSet<String> {
        self.allowed_tools
            .iter()
            .filter(|tool| self.tool_allowed(tool, read_only))
            .cloned()
            .collect()
    }
''',
)
replace_once(
    "crates/medusa-extensions/src/desktop_commander.rs",
    '''            Component::Normal(value) if value == std::ffi::OsStr::new(".medusa")
''',
    '''            Component::Normal(value)
                if value.to_string_lossy().eq_ignore_ascii_case(".medusa")
''',
)
replace_once(
    "crates/medusa-extensions/src/desktop_commander.rs",
    '''        assert!(sanitize_arguments(directory.path(), &json!({"path": "../secret"})).is_err());
    }
''',
    '''        assert!(sanitize_arguments(directory.path(), &json!({"path": "../secret"})).is_err());
        assert!(sanitize_arguments(
            directory.path(),
            &json!({"path": ".MEDUSA/sessions/private.json"}),
        )
        .is_err());
    }
''',
)
replace_once(
    "crates/medusa-extensions/src/desktop_commander.rs",
    '''    #[test]
    fn defaults_are_disabled_pinned_and_read_only() {
        let settings = DesktopCommanderSettings::default();
        assert!(!settings.requested());
        assert!(settings.args.iter().any(|arg| arg == PINNED_PACKAGE));
        assert!(settings.effective_tools().contains("read_file"));
        assert!(!settings.effective_tools().contains("write_file"));
        assert!(!settings.effective_tools().contains("start_process"));
    }
''',
    '''    #[test]
    fn defaults_and_read_only_mode_filter_write_tools() {
        let mut settings = DesktopCommanderSettings::default();
        assert!(!settings.requested());
        assert!(settings.args.iter().any(|arg| arg == PINNED_PACKAGE));
        assert!(settings.effective_tools().contains("read_file"));
        assert!(!settings.effective_tools().contains("start_process"));
        settings.enabled = true;
        settings.allow_write = true;
        settings.allowed_tools.insert("write_file".to_owned());
        assert!(settings.effective_tools_for_mode(false).contains("write_file"));
        assert!(!settings.effective_tools_for_mode(true).contains("write_file"));
    }
''',
)
replace_once(
    "crates/medusa-agent/src/tools/mod.rs",
    '''pub(crate) fn built_in_tools(desktop_commander: &DesktopCommanderSettings) -> Vec<ToolDefinition> {
''',
    '''pub(crate) fn built_in_tools(
    desktop_commander: &DesktopCommanderSettings,
    read_only: bool,
) -> Vec<ToolDefinition> {
''',
)
replace_once(
    "crates/medusa-agent/src/tools/mod.rs",
    '''            .effective_tools()
''',
    '''            .effective_tools_for_mode(read_only)
''',
)
replace_once(
    "crates/medusa-agent/src/engine_support.rs",
    '''    built_in_tools(desktop_commander)
''',
    '''    built_in_tools(desktop_commander, mode == Mode::ReadOnly)
''',
)
