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
    '''        let mut result =
            self.request("tools/call", json!({"name": tool, "arguments": arguments}))?;
        redact_value(&mut result);
        validate_mcp_output(&result)?;
        Ok(result)
''',
    '''        let mut result =
            self.request("tools/call", json!({"name": tool, "arguments": arguments}))?;
        validate_tool_result(tool, &mut result)?;
        Ok(result)
''',
)
replace_once(
    "crates/medusa-extensions/src/desktop_commander.rs",
    "fn spawn_reader(\n",
    '''fn validate_tool_result(tool: &str, result: &mut Value) -> MedusaResult<()> {
    redact_value(result);
    validate_mcp_output(result)?;
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(execution(format!(
            "Desktop Commander tool {tool} reported an error: {result}"
        )));
    }
    Ok(())
}

fn spawn_reader(
''',
)
replace_once(
    "crates/medusa-extensions/src/desktop_commander.rs",
    '''    #[cfg(unix)]
    #[test]
    fn persistent_stdio_client_initializes_discovers_and_calls() {
''',
    '''    #[test]
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
''',
)
replace_once(
    "crates/medusa-cli/src/main.rs",
    "use medusa_extensions::DesktopCommanderSettings;\n",
    "use medusa_extensions::{DesktopCommanderClient, DesktopCommanderSettings};\n",
)
replace_once(
    "crates/medusa-cli/src/main.rs",
    '''    checks.push(desktop_commander_check(
        &DesktopCommanderSettings::from_env(),
    ));
''',
    '''    checks.push(desktop_commander_check(
        repo,
        &DesktopCommanderSettings::from_env(),
    ));
''',
)
replace_once(
    "crates/medusa-cli/src/main.rs",
    "fn desktop_commander_check(settings: &DesktopCommanderSettings) -> DoctorCheck {\n",
    "fn desktop_commander_check(repo: &Path, settings: &DesktopCommanderSettings) -> DoctorCheck {\n",
)
replace_once(
    "crates/medusa-cli/src/main.rs",
    '''    let available = executable_available(settings.command());
    DoctorCheck {
        name: "desktop_commander_mcp",
        ok: available,
        detail: if available {
            format!(
                "{} via {}",
                settings.package_label(),
                settings.command().display()
            )
        } else {
            format!("{} was not found on PATH", settings.command().display())
        },
    }
''',
    '''    if !executable_available(settings.command()) {
        return DoctorCheck {
            name: "desktop_commander_mcp",
            ok: false,
            detail: format!("{} was not found on PATH", settings.command().display()),
        };
    }
    match DesktopCommanderClient::connect(repo, settings.clone()) {
        Ok(_) => DoctorCheck {
            name: "desktop_commander_mcp",
            ok: true,
            detail: format!(
                "MCP handshake ready: {} via {}",
                settings.package_label(),
                settings.command().display()
            ),
        },
        Err(error) => DoctorCheck {
            name: "desktop_commander_mcp",
            ok: false,
            detail: format!("MCP handshake failed: {error}"),
        },
    }
''',
)
replace_once(
    "README.md",
    "The integration uses a Medusa-owned isolated home under `.medusa/extensions/desktop-commander`, writes a Desktop Commander profile with telemetry and onboarding disabled, clears inherited credentials, and limits `allowedDirectories` to the active repository.",
    "When the integration is enabled, `medusa doctor` performs the real MCP initialize and tool-discovery handshake. The integration uses a Medusa-owned isolated home under `.medusa/extensions/desktop-commander`, writes a Desktop Commander profile with telemetry and onboarding disabled, clears inherited credentials, and limits `allowedDirectories` to the active repository.",
)
