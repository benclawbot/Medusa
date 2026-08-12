use super::*;

#[test]
fn defaults_and_read_only_mode_filter_write_tools() {
    let mut settings = DesktopCommanderSettings::default();
    assert!(!settings.requested());
    assert!(settings.args.iter().any(|arg| arg == PINNED_PACKAGE));
    assert!(settings.effective_tools().contains("read_file"));
    assert!(!settings.effective_tools().contains("start_process"));
    settings.enabled = true;
    assert!(
        !settings.enabled(),
        "Desktop Commander must not be advertised without process containment"
    );
    settings.allow_write = true;
    settings.allowed_tools.insert("write_file".to_owned());
    assert!(
        settings
            .effective_tools_for_mode(false)
            .contains("write_file")
    );
    assert!(
        !settings
            .effective_tools_for_mode(true)
            .contains("write_file")
    );
}

#[test]
fn path_policy_rewrites_relative_paths_and_denies_escape() {
    let directory = tempfile::tempdir().expect("tempdir");
    fs::write(directory.path().join("value.txt"), "42").expect("fixture");
    let safe = sanitize_arguments(
        directory.path(),
        &json!({"path": "value.txt", "options": {"outputPath": "result.pdf"}}),
    )
    .expect("safe arguments");
    assert!(
        safe["path"]
            .as_str()
            .expect("path")
            .starts_with(directory.path().to_str().expect("temp path"))
    );
    assert!(sanitize_arguments(directory.path(), &json!({"path": "../secret"})).is_err());
    assert!(
        sanitize_arguments(
            directory.path(),
            &json!({"path": ".MEDUSA/sessions/private.json"}),
        )
        .is_err()
    );
}

#[test]
fn process_meta_and_unknown_tools_fail_closed() {
    let mut settings = DesktopCommanderSettings {
        enabled: true,
        ..DesktopCommanderSettings::default()
    };
    settings.allowed_tools.extend([
        "start_process".to_owned(),
        "set_config_value".to_owned(),
        "future_mutating_tool".to_owned(),
    ]);
    assert!(!settings.tool_allowed("start_process", false));
    assert!(!settings.tool_allowed("set_config_value", false));
    assert!(!settings.tool_allowed("future_mutating_tool", false));
}

#[test]
fn mcp_error_result_is_not_recorded_as_success() {
    let mut result = json!({
        "isError": true,
        "content": [{"type": "text", "text": "permission denied"}]
    });
    assert!(validate_tool_result("write_file", &mut result).is_err());
}

#[test]
fn enabled_client_fails_closed_before_uncontained_process_spawn() {
    let repository = tempfile::tempdir().expect("repository");
    let outside = tempfile::tempdir().expect("outside directory");
    let escaped = outside.path().join("outside-desktop-commander.txt");
    let (command, args) = side_effect_command(&escaped);
    let settings = DesktopCommanderSettings {
        enabled: true,
        command,
        args,
        ..DesktopCommanderSettings::default()
    };

    let Err(error) = DesktopCommanderClient::connect(repository.path(), settings) else {
        panic!("uncontained Desktop Commander process must be blocked");
    };
    assert_eq!(error.code, ErrorCode::SandboxUnavailable);
    assert_eq!(
        error.context.get("required_boundary"),
        Some(&json!("os_process_containment"))
    );
    assert!(
        !escaped.exists(),
        "Desktop Commander process spawned outside containment"
    );
    assert!(!repository.path().join(".medusa").exists());
}

#[test]
fn disabled_client_preserves_default_configuration_error() {
    let directory = tempfile::tempdir().expect("tempdir");
    let Err(error) =
        DesktopCommanderClient::connect(directory.path(), DesktopCommanderSettings::default())
    else {
        panic!("Desktop Commander must remain disabled by default");
    };
    assert_eq!(error.code, ErrorCode::InvalidConfiguration);
    assert!(!directory.path().join(".medusa").exists());
}

fn side_effect_command(target: &Path) -> (PathBuf, Vec<String>) {
    #[cfg(windows)]
    {
        let command = env::var_os("ComSpec")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("cmd.exe"));
        let escaped = target.display().to_string().replace('"', "\"\"");
        (
            command,
            vec![
                "/D".into(),
                "/C".into(),
                format!("echo escaped>\"{escaped}\""),
            ],
        )
    }
    #[cfg(not(windows))]
    {
        let escaped = target.display().to_string().replace('\'', "'\\''");
        (
            PathBuf::from("/bin/sh"),
            vec!["-c".into(), format!("printf escaped > '{escaped}'")],
        )
    }
}
