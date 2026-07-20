use super::*;

#[test]
fn defaults_and_read_only_mode_filter_write_tools() {
    let mut settings = DesktopCommanderSettings::default();
    assert!(!settings.requested());
    assert!(settings.args.iter().any(|arg| arg == PINNED_PACKAGE));
    assert!(settings.effective_tools().contains("read_file"));
    assert!(!settings.effective_tools().contains("start_process"));
    settings.enabled = true;
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

#[cfg(unix)]
#[test]
fn persistent_stdio_client_initializes_discovers_and_calls() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("tempdir");
    let server = directory.path().join("fake-desktop-commander.sh");
    fs::write(
            &server,
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"fake-desktop-commander","version":"1.0.0"}}}'
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"read_file","description":"read fixture","inputSchema":{"type":"object"}}]}}'
      ;;
    *'"method":"tools/call"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"fixture-ok"}]}}'
      ;;
  esac
done
"#,
        )
        .expect("write fake server");
    let mut permissions = fs::metadata(&server).expect("metadata").permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&server, permissions).expect("set executable");
    fs::write(directory.path().join("value.txt"), "42").expect("write fixture");

    let settings = DesktopCommanderSettings {
        enabled: true,
        command: server,
        args: Vec::new(),
        allowed_tools: BTreeSet::from(["read_file".to_owned()]),
        allow_write: false,
        timeout: Duration::from_secs(2),
        max_output_bytes: 16 * 1024,
        configuration_error: None,
    };
    let mut client =
        DesktopCommanderClient::connect(directory.path(), settings).expect("connect fake MCP");
    let result = client
        .call_tool(
            directory.path(),
            "read_file",
            &json!({"path": "value.txt"}),
            false,
        )
        .expect("call fake MCP tool");
    assert_eq!(result["content"][0]["text"], "fixture-ok");

    let profile = directory
        .path()
        .join(".medusa/extensions/desktop-commander/home/.claude-server-commander/config.json");
    let profile: Value =
        serde_json::from_slice(&fs::read(profile).expect("read profile")).expect("profile JSON");
    assert_eq!(profile["telemetryEnabled"], false);
    assert_eq!(
        profile["allowedDirectories"][0],
        directory.path().display().to_string()
    );
}
