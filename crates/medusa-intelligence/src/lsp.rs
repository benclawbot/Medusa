use std::{
    collections::BTreeMap,
    fmt,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Configuration for one language-server process.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LspServerConfig {
    pub language: String,
    pub command: String,
    pub args: Vec<String>,
    pub workspace_root: PathBuf,
    pub initialization_options: Option<Value>,
}

impl LspServerConfig {
    #[must_use]
    pub fn rust_analyzer(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            language: "rust".to_owned(),
            command: "rust-analyzer".to_owned(),
            args: Vec::new(),
            workspace_root: workspace_root.into(),
            initialization_options: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LspServerState {
    Stopped,
    Starting,
    Running,
    ShuttingDown,
    Failed,
}

#[derive(Debug)]
pub enum LspError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Protocol(String),
    ServerExited,
}

impl fmt::Display for LspError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "LSP I/O error: {error}"),
            Self::Json(error) => write!(formatter, "LSP JSON error: {error}"),
            Self::Protocol(message) => write!(formatter, "LSP protocol error: {message}"),
            Self::ServerExited => formatter.write_str("language server exited unexpectedly"),
        }
    }
}

impl std::error::Error for LspError {}

impl From<std::io::Error> for LspError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for LspError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Minimal synchronous JSON-RPC client over LSP stdio framing.
pub struct LspClient {
    config: LspServerConfig,
    state: LspServerState,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<BufReader<ChildStdout>>,
    next_request_id: u64,
    pending_notifications: Vec<Value>,
}

impl LspClient {
    #[must_use]
    pub fn new(config: LspServerConfig) -> Self {
        Self {
            config,
            state: LspServerState::Stopped,
            child: None,
            stdin: None,
            stdout: None,
            next_request_id: 1,
            pending_notifications: Vec::new(),
        }
    }

    #[must_use]
    pub fn state(&self) -> LspServerState {
        self.state
    }

    #[must_use]
    pub fn config(&self) -> &LspServerConfig {
        &self.config
    }

    pub fn start(&mut self) -> Result<Value, LspError> {
        if self.state == LspServerState::Running {
            return Err(LspError::Protocol(
                "language server is already running".to_owned(),
            ));
        }
        self.state = LspServerState::Starting;

        let mut command = Command::new(&self.config.command);
        command
            .args(&self.config.args)
            .current_dir(&self.config.workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.state = LspServerState::Failed;
                return Err(error.into());
            }
        };
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| LspError::Protocol("language server stdin is unavailable".to_owned()))?;
        let stdout = child.stdout.take().ok_or_else(|| {
            LspError::Protocol("language server stdout is unavailable".to_owned())
        })?;

        self.stdin = Some(stdin);
        self.stdout = Some(BufReader::new(stdout));
        self.child = Some(child);

        let root_uri = path_to_file_uri(&self.config.workspace_root);
        let mut params = json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "capabilities": {},
            "clientInfo": { "name": "medusa", "version": env!("CARGO_PKG_VERSION") }
        });
        if let Some(options) = &self.config.initialization_options {
            params["initializationOptions"] = options.clone();
        }

        let response = self.request("initialize", params)?;
        self.notify("initialized", json!({}))?;
        self.state = LspServerState::Running;
        Ok(response)
    }

    pub fn request(&mut self, method: &str, params: Value) -> Result<Value, LspError> {
        let id = self.next_request_id;
        self.next_request_id += 1;
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))?;

        loop {
            let message = self.read_message()?;
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(error) = message.get("error") {
                    return Err(LspError::Protocol(error.to_string()));
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
            self.pending_notifications.push(message);
        }
    }

    pub fn notify(&mut self, method: &str, params: Value) -> Result<(), LspError> {
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        }))
    }

    #[must_use]
    pub fn drain_notifications(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.pending_notifications)
    }

    pub fn shutdown(&mut self) -> Result<(), LspError> {
        if self.state == LspServerState::Stopped {
            return Ok(());
        }
        self.state = LspServerState::ShuttingDown;
        let _ = self.request("shutdown", Value::Null);
        let _ = self.notify("exit", Value::Null);

        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
        self.stdin = None;
        self.stdout = None;
        self.state = LspServerState::Stopped;
        Ok(())
    }

    fn write_message(&mut self, message: &Value) -> Result<(), LspError> {
        let body = serde_json::to_vec(message)?;
        let stdin = self.stdin.as_mut().ok_or(LspError::ServerExited)?;
        write!(stdin, "Content-Length: {}\r\n\r\n", body.len())?;
        stdin.write_all(&body)?;
        stdin.flush()?;
        Ok(())
    }

    fn read_message(&mut self) -> Result<Value, LspError> {
        let stdout = self.stdout.as_mut().ok_or(LspError::ServerExited)?;
        read_lsp_message(stdout)
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// Owns one reusable LSP client per language/workspace pair.
#[derive(Default)]
pub struct LspServerManager {
    servers: BTreeMap<(String, PathBuf), LspClient>,
}

impl LspServerManager {
    pub fn start(&mut self, config: LspServerConfig) -> Result<&mut LspClient, LspError> {
        let key = (config.language.clone(), config.workspace_root.clone());
        let client = self
            .servers
            .entry(key)
            .or_insert_with(|| LspClient::new(config));
        if client.state() != LspServerState::Running {
            client.start()?;
        }
        Ok(client)
    }

    #[must_use]
    pub fn get_mut(&mut self, language: &str, root: &Path) -> Option<&mut LspClient> {
        self.servers
            .get_mut(&(language.to_owned(), root.to_path_buf()))
    }

    pub fn stop(&mut self, language: &str, root: &Path) -> Result<(), LspError> {
        if let Some(mut client) = self
            .servers
            .remove(&(language.to_owned(), root.to_path_buf()))
        {
            client.shutdown()?;
        }
        Ok(())
    }

    pub fn stop_all(&mut self) -> Result<(), LspError> {
        for client in self.servers.values_mut() {
            client.shutdown()?;
        }
        self.servers.clear();
        Ok(())
    }
}

fn read_lsp_message(reader: &mut impl BufRead) -> Result<Value, LspError> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(LspError::ServerExited);
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| LspError::Protocol("invalid Content-Length header".to_owned()))?,
            );
        }
    }

    let length = content_length
        .ok_or_else(|| LspError::Protocol("missing Content-Length header".to_owned()))?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body)?)
}

fn path_to_file_uri(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");
    if normalized.starts_with('/') {
        format!("file://{normalized}")
    } else {
        format!("file:///{normalized}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn decodes_lsp_content_length_frames() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        let framed = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        let decoded = read_lsp_message(&mut Cursor::new(framed.into_bytes())).expect("message");
        assert_eq!(decoded["id"], 1);
    }

    #[test]
    fn manager_keys_servers_by_language_and_workspace() {
        let root = PathBuf::from("/workspace");
        let mut manager = LspServerManager::default();
        manager.servers.insert(
            ("rust".to_owned(), root.clone()),
            LspClient::new(LspServerConfig::rust_analyzer(&root)),
        );
        assert!(manager.get_mut("rust", &root).is_some());
        assert!(manager.get_mut("python", &root).is_none());
    }

    #[test]
    fn file_uri_normalizes_windows_separators() {
        assert_eq!(path_to_file_uri(Path::new("C:\\repo")), "file:///C:/repo");
        assert_eq!(path_to_file_uri(Path::new("/repo")), "file:///repo");
    }
}
