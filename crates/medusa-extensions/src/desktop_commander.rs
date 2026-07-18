use std::{
    collections::BTreeSet,
    env, fs,
    io::{self, BufRead, BufReader, Write},
    path::{Component, Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde_json::{Value, json};

use crate::{mcp::validate_mcp_output, redaction::redact_value};

const PINNED_PACKAGE: &str = "@wonderwhy-er/desktop-commander@0.2.46";
const DEFAULT_READ_TOOLS: &[&str] = &[
    "read_file",
    "read_multiple_files",
    "list_directory",
    "get_file_info",
    "start_search",
    "get_more_search_results",
    "stop_search",
    "list_searches",
];
const WRITE_TOOLS: &[&str] = &[
    "write_file",
    "create_directory",
    "move_file",
    "edit_block",
    "write_pdf",
];
const PROCESS_TOOLS: &[&str] = &[
    "start_process",
    "read_process_output",
    "interact_with_process",
    "force_terminate",
    "list_sessions",
    "kill_process",
    "list_processes",
];
const FORBIDDEN_META_TOOLS: &[&str] = &[
    "get_config",
    "set_config_value",
    "get_usage_stats",
    "give_feedback_to_desktop_commander",
    "get_prompts",
    "get_recent_tool_calls",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopCommanderSettings {
    enabled: bool,
    command: PathBuf,
    args: Vec<String>,
    allowed_tools: BTreeSet<String>,
    allow_write: bool,
    timeout: Duration,
    max_output_bytes: usize,
    configuration_error: Option<String>,
}

impl Default for DesktopCommanderSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            command: PathBuf::from(if cfg!(windows) { "npx.cmd" } else { "npx" }),
            args: vec![
                "-y".to_owned(),
                PINNED_PACKAGE.to_owned(),
                "--no-onboarding".to_owned(),
            ],
            allowed_tools: DEFAULT_READ_TOOLS
                .iter()
                .map(|tool| (*tool).to_owned())
                .collect(),
            allow_write: false,
            timeout: Duration::from_secs(30),
            max_output_bytes: 256 * 1024,
            configuration_error: None,
        }
    }
}

impl DesktopCommanderSettings {
    #[must_use]
    pub fn from_env() -> Self {
        let mut settings = Self {
            enabled: env_flag("MEDUSA_DESKTOP_COMMANDER_ENABLED"),
            ..Self::default()
        };
        if let Ok(command) = env::var("MEDUSA_DESKTOP_COMMANDER_COMMAND") {
            if !command.trim().is_empty() {
                settings.command = PathBuf::from(command);
            }
        }
        if let Ok(raw) = env::var("MEDUSA_DESKTOP_COMMANDER_ARGS") {
            match serde_json::from_str::<Vec<String>>(&raw) {
                Ok(args) if !args.is_empty() => settings.args = args,
                Ok(_) => {
                    settings.configuration_error = Some(
                        "MEDUSA_DESKTOP_COMMANDER_ARGS must contain at least one argument"
                            .to_owned(),
                    );
                }
                Err(error) => {
                    settings.configuration_error = Some(format!(
                        "MEDUSA_DESKTOP_COMMANDER_ARGS must be a JSON string array: {error}"
                    ));
                }
            }
        }
        if let Ok(raw) = env::var("MEDUSA_DESKTOP_COMMANDER_ALLOWED_TOOLS") {
            let allowed = csv_set(&raw);
            if !allowed.is_empty() {
                settings.allowed_tools = allowed;
            }
        }
        settings.allow_write = env_flag("MEDUSA_DESKTOP_COMMANDER_ALLOW_WRITE");
        settings.timeout =
            Duration::from_millis(env_u64("MEDUSA_DESKTOP_COMMANDER_TIMEOUT_MS", 30_000));
        settings.max_output_bytes =
            env_usize("MEDUSA_DESKTOP_COMMANDER_MAX_OUTPUT_BYTES", 256 * 1024).max(1024);
        if settings.enabled
            && settings.configuration_error.is_none()
            && settings.effective_tools().is_empty()
        {
            settings.configuration_error = Some(
                "Desktop Commander is enabled but no policy-approved tools are available"
                    .to_owned(),
            );
        }
        settings
    }

    #[must_use]
    pub fn requested(&self) -> bool {
        self.enabled
    }

    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled && self.configuration_error.is_none()
    }

    #[must_use]
    pub fn command(&self) -> &Path {
        &self.command
    }

    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }

    #[must_use]
    pub fn configuration_error(&self) -> Option<&str> {
        self.configuration_error.as_deref()
    }

    #[must_use]
    pub fn package_label(&self) -> String {
        self.args
            .iter()
            .find(|arg| arg.contains("desktop-commander@"))
            .cloned()
            .unwrap_or_else(|| self.command.display().to_string())
    }

    #[must_use]
    pub fn effective_tools(&self) -> BTreeSet<String> {
        self.allowed_tools
            .iter()
            .filter(|tool| self.tool_allowed(tool, false))
            .cloned()
            .collect()
    }

    fn tool_allowed(&self, tool: &str, read_only: bool) -> bool {
        if FORBIDDEN_META_TOOLS.contains(&tool) || !self.allowed_tools.contains(tool) {
            return false;
        }
        if desktop_commander_tool_is_mutating(tool) && (!self.allow_write || read_only) {
            return false;
        }
        if is_process_tool(tool) {
            return false;
        }
        DEFAULT_READ_TOOLS.contains(&tool) || WRITE_TOOLS.contains(&tool)
    }
}

pub struct DesktopCommanderClient {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Result<Value, String>>,
    next_id: u64,
    discovered_tools: BTreeSet<String>,
    settings: DesktopCommanderSettings,
}

impl DesktopCommanderClient {
    pub fn connect(repo: &Path, settings: DesktopCommanderSettings) -> MedusaResult<Self> {
        if !settings.enabled() {
            return Err(invalid(
                settings
                    .configuration_error()
                    .unwrap_or("Desktop Commander MCP is not enabled"),
            ));
        }
        let repo = repo.canonicalize()?;
        let state = repo.join(".medusa/extensions/desktop-commander");
        let home = state.join("home");
        prepare_profile(&repo, &home)?;

        let mut command = Command::new(settings.command());
        command
            .args(settings.args())
            .current_dir(&repo)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        copy_safe_environment(&mut command);
        let app_data = home.join("appdata");
        let cache = state.join("npm-cache");
        fs::create_dir_all(&app_data)?;
        fs::create_dir_all(&cache)?;
        command
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("APPDATA", &app_data)
            .env("LOCALAPPDATA", &app_data)
            .env("npm_config_cache", &cache)
            .env("npm_config_ignore_scripts", "true")
            .env("npm_config_audit", "false")
            .env("npm_config_fund", "false")
            .env("DO_NOT_TRACK", "1")
            .env("NO_COLOR", "1")
            .env("CI", "1");

        let mut child = command.spawn().map_err(|error| {
            execution(format!(
                "start Desktop Commander with {}: {error}",
                settings.command().display()
            ))
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| execution("Desktop Commander stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| execution("Desktop Commander stdout unavailable"))?;
        if let Some(mut stderr) = child.stderr.take() {
            thread::spawn(move || {
                let _ = io::copy(&mut stderr, &mut io::sink());
            });
        }
        let messages = spawn_reader(stdout, settings.max_output_bytes);
        let mut client = Self {
            child,
            stdin,
            messages,
            next_id: 1,
            discovered_tools: BTreeSet::new(),
            settings,
        };
        client.initialize()?;
        Ok(client)
    }

    pub fn call_tool(
        &mut self,
        repo: &Path,
        tool: &str,
        arguments: &Value,
        read_only: bool,
    ) -> MedusaResult<Value> {
        if !self.discovered_tools.contains(tool) {
            return Err(invalid(format!(
                "Desktop Commander did not advertise tool {tool}"
            )));
        }
        if !self.settings.tool_allowed(tool, read_only) {
            return Err(policy(format!(
                "Desktop Commander tool {tool} is outside Medusa's configured capability policy"
            )));
        }
        let arguments = sanitize_arguments(repo, arguments)?;
        let mut result =
            self.request("tools/call", json!({"name": tool, "arguments": arguments}))?;
        redact_value(&mut result);
        validate_mcp_output(&result)?;
        Ok(result)
    }

    fn initialize(&mut self) -> MedusaResult<()> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "medusa", "version": env!("CARGO_PKG_VERSION")}
            }),
        )?;
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))?;
        let result = self.request("tools/list", json!({}))?;
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| execution("Desktop Commander tools/list returned no tools array"))?;
        self.discovered_tools = tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .map(str::to_owned)
            .collect();
        if self.discovered_tools.is_empty() {
            return Err(execution("Desktop Commander advertised no MCP tools"));
        }
        Ok(())
    }

    fn request(&mut self, method: &str, params: Value) -> MedusaResult<Value> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))?;
        let deadline = Instant::now() + self.settings.timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(execution(format!(
                    "Desktop Commander timed out waiting for {method}"
                )));
            }
            let message = match self.messages.recv_timeout(remaining) {
                Ok(Ok(message)) => message,
                Ok(Err(error)) => return Err(execution(error)),
                Err(RecvTimeoutError::Timeout) => {
                    return Err(execution(format!(
                        "Desktop Commander timed out waiting for {method}"
                    )));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(execution("Desktop Commander output stream closed"));
                }
            };
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(execution(format!(
                    "Desktop Commander {method} failed: {error}"
                )));
            }
            return message.get("result").cloned().ok_or_else(|| {
                execution(format!("Desktop Commander {method} returned no result"))
            });
        }
    }

    fn send(&mut self, value: Value) -> MedusaResult<()> {
        serde_json::to_writer(&mut self.stdin, &value)?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }
}

impl Drop for DesktopCommanderClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[must_use]
pub fn desktop_commander_tool_is_mutating(tool: &str) -> bool {
    WRITE_TOOLS.contains(&tool)
}

fn is_process_tool(tool: &str) -> bool {
    PROCESS_TOOLS.contains(&tool)
}

fn spawn_reader(
    stdout: impl io::Read + Send + 'static,
    max_output_bytes: usize,
) -> Receiver<Result<Value, String>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut buffer = Vec::new();
            match reader.read_until(b'\n', &mut buffer) {
                Ok(0) => break,
                Ok(_) if buffer.len() > max_output_bytes => {
                    let _ = sender.send(Err(format!(
                        "Desktop Commander response exceeded {max_output_bytes} bytes"
                    )));
                    break;
                }
                Ok(_) => {
                    while matches!(buffer.last(), Some(b'\n' | b'\r')) {
                        buffer.pop();
                    }
                    if buffer.is_empty() {
                        continue;
                    }
                    match serde_json::from_slice(&buffer) {
                        Ok(value) => {
                            if sender.send(Ok(value)).is_err() {
                                break;
                            }
                        }
                        Err(error) => {
                            let _ = sender.send(Err(format!(
                                "Desktop Commander emitted invalid JSON: {error}"
                            )));
                            break;
                        }
                    }
                }
                Err(error) => {
                    let _ = sender.send(Err(format!("read Desktop Commander output: {error}")));
                    break;
                }
            }
        }
    });
    receiver
}

fn prepare_profile(repo: &Path, home: &Path) -> MedusaResult<()> {
    let config_dir = home.join(".claude-server-commander");
    fs::create_dir_all(&config_dir)?;
    let config = json!({
        "blockedCommands": [
            "rm", "sudo", "shutdown", "reboot", "mkfs", "format", "mount", "umount",
            "dd", "fdisk", "parted", "diskpart", "su", "passwd", "useradd", "usermod",
            "iptables", "netsh", "reg", "sc", "runas", "cipher", "takeown"
        ],
        "allowedDirectories": [repo.display().to_string()],
        "telemetryEnabled": false,
        "pendingWelcomeOnboarding": false,
        "welcomeOnboardingEligible": false,
        "fileReadLineLimit": 1000,
        "fileWriteLineLimit": 200
    });
    fs::write(
        config_dir.join("config.json"),
        serde_json::to_vec_pretty(&config)?,
    )?;
    Ok(())
}

fn copy_safe_environment(command: &mut Command) {
    for key in [
        "PATH",
        "Path",
        "PATHEXT",
        "SystemRoot",
        "ComSpec",
        "WINDIR",
        "TEMP",
        "TMP",
        "TMPDIR",
        "SHELL",
    ] {
        if let Some(value) = env::var_os(key) {
            command.env(key, value);
        }
    }
}

fn sanitize_arguments(repo: &Path, arguments: &Value) -> MedusaResult<Value> {
    if !arguments.is_object() {
        return Err(invalid("Desktop Commander arguments must be a JSON object"));
    }
    let root = repo.canonicalize()?;
    let mut safe = arguments.clone();
    rewrite_paths(&root, &mut safe, None)?;
    Ok(safe)
}

fn rewrite_paths(root: &Path, value: &mut Value, key: Option<&str>) -> MedusaResult<()> {
    match value {
        Value::String(raw) if key.is_some_and(is_path_key) => {
            *raw = secure_path(root, raw)?.display().to_string();
        }
        Value::Array(values) if key.is_some_and(is_path_key) => {
            for value in values {
                let raw = value
                    .as_str()
                    .ok_or_else(|| invalid("every Desktop Commander path must be a string"))?;
                *value = Value::String(secure_path(root, raw)?.display().to_string());
            }
        }
        Value::Array(values) => {
            for value in values {
                rewrite_paths(root, value, key)?;
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                rewrite_paths(root, value, Some(key))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn is_path_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("path") || matches!(key.as_str(), "source" | "destination" | "directory")
}

fn secure_path(root: &Path, raw: &str) -> MedusaResult<PathBuf> {
    if raw.trim().is_empty() || raw.starts_with('~') || raw.contains('\0') {
        return Err(policy(
            "Desktop Commander path is empty, contains NUL, or uses ~ expansion",
        ));
    }
    let path = Path::new(raw);
    if path
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err(policy("Desktop Commander parent path traversal is denied"));
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::Normal(value) if value == std::ffi::OsStr::new(".medusa")
        )
    }) {
        return Err(policy("Desktop Commander access to Medusa state is denied"));
    }
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    if !candidate.starts_with(root) {
        return Err(policy(format!(
            "Desktop Commander path escapes the repository: {}",
            candidate.display()
        )));
    }
    let mut probe = candidate.as_path();
    while !probe.exists() {
        probe = probe
            .parent()
            .ok_or_else(|| policy("Desktop Commander path has no existing ancestor"))?;
    }
    let canonical_probe = probe.canonicalize()?;
    if !canonical_probe.starts_with(root) {
        return Err(policy(format!(
            "Desktop Commander path crosses a symlink outside the repository: {}",
            candidate.display()
        )));
    }
    if candidate.exists() && !candidate.canonicalize()?.starts_with(root) {
        return Err(policy(format!(
            "Desktop Commander path resolves outside the repository: {}",
            candidate.display()
        )));
    }
    Ok(candidate)
}

fn env_flag(key: &str) -> bool {
    env::var(key).ok().is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn env_u64(key: &str, default: u64) -> u64 {
    env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn csv_set(raw: &str) -> BTreeSet<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

fn policy(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::PolicyDenied, ErrorCategory::Policy, message)
}

fn execution(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Execution,
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_disabled_pinned_and_read_only() {
        let settings = DesktopCommanderSettings::default();
        assert!(!settings.requested());
        assert!(settings.args.iter().any(|arg| arg == PINNED_PACKAGE));
        assert!(settings.effective_tools().contains("read_file"));
        assert!(!settings.effective_tools().contains("write_file"));
        assert!(!settings.effective_tools().contains("start_process"));
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
        let profile: Value = serde_json::from_slice(&fs::read(profile).expect("read profile"))
            .expect("profile JSON");
        assert_eq!(profile["telemetryEnabled"], false);
        assert_eq!(
            profile["allowedDirectories"][0],
            directory.path().display().to_string()
        );
    }
}
