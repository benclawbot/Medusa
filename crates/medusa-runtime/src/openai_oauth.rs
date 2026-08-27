use std::{
    collections::VecDeque,
    io::{self, BufRead, BufReader, BufWriter, Write},
    path::Path,
    process::{Child, ChildStdin, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use medusa_core::hidden_command;
use serde_json::{Value, json};

const PROTOCOL_TIMEOUT: Duration = Duration::from_secs(30);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
const TURN_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const READ_POLL_INTERVAL: Duration = Duration::from_millis(250);
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
const APP_SERVER_TIMEOUT: &str = "timed out waiting for Codex app-server";
// OAuth credentials are owned by the first-party Codex service; do not inherit
// a user's unrelated local router endpoint from the Codex CLI configuration.
const DIRECT_CHATGPT_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

#[derive(Clone, Debug)]
pub struct PendingServerRequest {
    pub id: Value,
    pub method: String,
    pub params: Value,
}

#[derive(Clone, Debug)]
pub enum CodexTurnEvent {
    AssistantDelta(String),
    Activity { method: String, params: Value },
    Plan(Value),
    Usage(Value),
    Approval(PendingServerRequest),
    Completed(CodexTurnCompletion),
}

#[derive(Clone, Debug)]
pub struct CodexTurnCompletion {
    pub status: String,
    pub text: String,
    pub usage: Option<Value>,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingDecision {
    Accept,
    AcceptForSession,
    Decline,
    Cancel,
}

pub struct OpenAiOAuthLogin {
    receiver: Receiver<Result<Vec<String>, String>>,
    cancel: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl OpenAiOAuthLogin {
    pub fn poll(&mut self) -> Option<Result<Vec<String>, String>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(
                "Codex browser sign-in task exited before reporting a result".to_owned(),
            )),
        }
    }

    pub fn cancel(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for OpenAiOAuthLogin {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub struct CodexAppServer {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    incoming: Receiver<Result<Value, String>>,
    notifications: VecDeque<Value>,
    pending_request: Option<PendingServerRequest>,
    next_id: u64,
    initialized: bool,
    last_usage: Option<Value>,
    active_thread_id: Option<String>,
    active_turn_id: Option<String>,
}

impl Drop for CodexAppServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl CodexAppServer {
    pub fn connect() -> Result<Self, String> {
        let mut child = hidden_command(codex_program())
            .args(codex_app_server_args())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                format!(
                    "could not launch `{} app-server --stdio`: {error}. Install the Codex CLI and ensure it is on PATH",
                    codex_program()
                )
            })?;
        let Some(stdout) = child.stdout.take() else {
            terminate_child(&mut child);
            return Err("Codex app-server did not expose stdout".to_owned());
        };
        let Some(stdin) = child.stdin.take() else {
            terminate_child(&mut child);
            return Err("Codex app-server did not expose stdin".to_owned());
        };
        let (sender, receiver) = mpsc::channel();
        if let Err(error) = thread::Builder::new()
            .name("medusa-codex-app-server-reader".to_owned())
            .spawn(move || read_protocol_lines(stdout, sender))
        {
            terminate_child(&mut child);
            return Err(format!(
                "could not monitor Codex app-server output: {error}"
            ));
        }
        let mut server = Self {
            child,
            stdin: BufWriter::new(stdin),
            incoming: receiver,
            notifications: VecDeque::new(),
            pending_request: None,
            next_id: 1,
            initialized: false,
            last_usage: None,
            active_thread_id: None,
            active_turn_id: None,
        };
        server.initialize()?;
        Ok(server)
    }

    fn initialize(&mut self) -> Result<(), String> {
        if self.initialized {
            return Ok(());
        }
        self.request(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "medusa",
                    "title": "Medusa",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": {}
            }),
        )?;
        self.notify("initialized", json!({}))?;
        self.initialized = true;
        Ok(())
    }

    pub fn ensure_authenticated(&mut self) -> Result<(), String> {
        self.ensure_authenticated_with_cancel(None)
    }

    fn ensure_authenticated_with_cancel(
        &mut self,
        cancel: Option<&AtomicBool>,
    ) -> Result<(), String> {
        let account = self.account_read(true)?;
        if account_type(&account) == Some("chatgpt") {
            return Ok(());
        }
        if account_type(&account).is_some() {
            return Err(
                "Codex is signed in with a non-ChatGPT account; select a ChatGPT account for the openai-oauth provider".to_owned(),
            );
        }
        let login = self.request("account/login/start", json!({"type": "chatgpt"}))?;
        let auth_url = login
            .get("authUrl")
            .and_then(Value::as_str)
            .filter(|url| url.starts_with("https://") || url.starts_with("http://"))
            .ok_or_else(|| "Codex did not return a valid ChatGPT sign-in URL".to_owned())?;
        let login_id = login
            .get("loginId")
            .and_then(Value::as_str)
            .ok_or_else(|| "Codex did not return a ChatGPT sign-in id".to_owned())?
            .to_owned();
        open_browser_url(auth_url)?;
        self.wait_for_login(&login_id, cancel)?;
        if account_type(&self.account_read(true)?) == Some("chatgpt") {
            Ok(())
        } else {
            Err(
                "ChatGPT sign-in completed, but Codex did not report an authenticated account"
                    .to_owned(),
            )
        }
    }

    pub fn list_models(&mut self) -> Result<Vec<String>, String> {
        let result = self.request("model/list", json!({"includeHidden": false}))?;
        parse_models(result)
    }

    pub fn start_or_resume_thread(
        &mut self,
        cwd: &Path,
        model: &str,
        approval_policy: &str,
        sandbox: &str,
        thread_id: Option<&str>,
    ) -> Result<String, String> {
        let params = if let Some(thread_id) = thread_id {
            json!({
                "threadId": thread_id,
                "cwd": cwd,
                "model": model,
                "approvalPolicy": approval_policy,
                "sandbox": sandbox,
                "excludeTurns": true
            })
        } else {
            json!({
                "cwd": cwd,
                "model": model,
                "approvalPolicy": approval_policy,
                "sandbox": sandbox
            })
        };
        let method = if thread_id.is_some() {
            "thread/resume"
        } else {
            "thread/start"
        };
        let result = self.request(method, params)?;
        let thread_id = result
            .get("thread")
            .and_then(|thread| thread.get("id"))
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("Codex {method} returned no thread id"))?;
        self.active_thread_id = Some(thread_id.clone());
        Ok(thread_id)
    }

    pub fn start_turn(
        &mut self,
        thread_id: &str,
        input: Vec<Value>,
        model: &str,
        effort: &str,
        cwd: &Path,
        sandbox: &str,
    ) -> Result<String, String> {
        self.last_usage = None;
        let result = self.request(
            "turn/start",
            json!({
                "threadId": thread_id,
                "input": input,
                "model": model,
                "effort": effort,
                "cwd": cwd,
                "sandboxPolicy": {"type": sandbox}
            }),
        )?;
        let turn_id = result
            .get("turn")
            .and_then(|turn| turn.get("id"))
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| "Codex turn/start returned no turn id".to_owned())?;
        self.active_thread_id = Some(thread_id.to_owned());
        self.active_turn_id = Some(turn_id.clone());
        Ok(turn_id)
    }

    pub fn active_turn(&self) -> Option<(&str, &str)> {
        Some((
            self.active_thread_id.as_deref()?,
            self.active_turn_id.as_deref()?,
        ))
    }

    pub fn interrupt(&mut self, thread_id: &str, turn_id: &str) -> Result<(), String> {
        self.request(
            "turn/interrupt",
            json!({"threadId": thread_id, "turnId": turn_id}),
        )?;
        Ok(())
    }

    pub fn next_turn_event_with_cancel(
        &mut self,
        cancel: Option<&AtomicBool>,
    ) -> Result<CodexTurnEvent, String> {
        let deadline = Instant::now() + TURN_TIMEOUT;
        loop {
            if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
                return Err("ChatGPT OAuth turn cancelled".to_owned());
            }
            if let Some(pending) = self.pending_request.clone() {
                return Ok(CodexTurnEvent::Approval(pending));
            }
            let poll_deadline = std::cmp::min(deadline, Instant::now() + READ_POLL_INTERVAL);
            let message = match self.next_message(poll_deadline) {
                Ok(message) => message,
                Err(error) if error == APP_SERVER_TIMEOUT && Instant::now() < deadline => continue,
                Err(error) => return Err(error),
            };
            if let Some(method) = message.get("method").and_then(Value::as_str) {
                if let Some(id) = message.get("id") {
                    if is_user_interaction_request(method) {
                        let pending = PendingServerRequest {
                            id: id.clone(),
                            method: method.to_owned(),
                            params: message.get("params").cloned().unwrap_or(Value::Null),
                        };
                        self.pending_request = Some(pending.clone());
                        return Ok(CodexTurnEvent::Approval(pending));
                    }
                    self.send_error(id.clone(), -32601, "unsupported Codex app-server request")?;
                    continue;
                }
                match method {
                    "item/agentMessage/delta" => {
                        if let Some(delta) = message
                            .get("params")
                            .and_then(|params| params.get("delta"))
                            .and_then(Value::as_str)
                        {
                            if !delta.is_empty() {
                                return Ok(CodexTurnEvent::AssistantDelta(delta.to_owned()));
                            }
                        }
                    }
                    "thread/tokenUsage/updated" => {
                        let usage = message
                            .get("params")
                            .and_then(|params| params.get("tokenUsage"))
                            .cloned()
                            .unwrap_or(Value::Null);
                        self.last_usage = Some(usage.clone());
                        return Ok(CodexTurnEvent::Usage(usage));
                    }
                    "turn/plan/updated" => {
                        return Ok(CodexTurnEvent::Plan(
                            message
                                .get("params")
                                .and_then(|params| params.get("plan"))
                                .cloned()
                                .unwrap_or(Value::Null),
                        ));
                    }
                    "turn/completed" => {
                        let turn = message
                            .get("params")
                            .and_then(|params| params.get("turn"))
                            .cloned()
                            .unwrap_or(Value::Null);
                        let status = turn
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("failed")
                            .to_owned();
                        let error = turn
                            .get("error")
                            .and_then(|error| error.get("message").or(Some(error)))
                            .and_then(Value::as_str)
                            .map(|message| safe_error_text(&Value::String(message.to_owned())));
                        self.active_turn_id = None;
                        return Ok(CodexTurnEvent::Completed(CodexTurnCompletion {
                            status,
                            text: extract_agent_text(&turn),
                            usage: self.last_usage.take(),
                            error,
                        }));
                    }
                    "item/started"
                    | "item/completed"
                    | "item/commandExecution/outputDelta"
                    | "item/fileChange/outputDelta" => {
                        return Ok(CodexTurnEvent::Activity {
                            method: method.to_owned(),
                            params: message.get("params").cloned().unwrap_or(Value::Null),
                        });
                    }
                    _ => {}
                }
            }
        }
    }

    pub fn respond_pending_with_answer(&mut self, answer: &str) -> Result<(), String> {
        let pending = self.pending_request.take().ok_or_else(|| {
            "there is no pending Codex app-server approval or question to answer".to_owned()
        })?;
        let normalized = answer.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            self.pending_request = Some(pending);
            return Err("a Codex approval response cannot be empty".to_owned());
        }
        let decision = if normalized == "approve for session"
            || normalized == "accept for session"
            || normalized == "yes for session"
        {
            PendingDecision::AcceptForSession
        } else if normalized == "approve"
            || normalized == "accept"
            || normalized == "yes"
            || normalized == "y"
        {
            PendingDecision::Accept
        } else if normalized == "cancel" {
            PendingDecision::Cancel
        } else {
            PendingDecision::Decline
        };
        let result = self.respond_pending(pending.clone(), decision, answer);
        if result.is_err() {
            self.pending_request = Some(pending);
        }
        result
    }

    fn respond_pending(
        &mut self,
        pending: PendingServerRequest,
        decision: PendingDecision,
        answer: &str,
    ) -> Result<(), String> {
        let result = match pending.method.as_str() {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                let decision = match decision {
                    PendingDecision::Accept => "accept",
                    PendingDecision::AcceptForSession => "acceptForSession",
                    PendingDecision::Decline => "decline",
                    PendingDecision::Cancel => "cancel",
                };
                json!({"decision": decision})
            }
            "item/permissions/requestApproval" => {
                let permissions = if matches!(
                    decision,
                    PendingDecision::Accept | PendingDecision::AcceptForSession
                ) {
                    pending
                        .params
                        .get("permissions")
                        .cloned()
                        .unwrap_or_else(|| json!({}))
                } else {
                    json!({})
                };
                json!({
                    "permissions": permissions,
                    "scope": if decision == PendingDecision::AcceptForSession { "session" } else { "turn" }
                })
            }
            "item/tool/requestUserInput" => {
                let answers = if decision == PendingDecision::Decline
                    || decision == PendingDecision::Cancel
                {
                    json!({})
                } else {
                    let mut answers = serde_json::Map::new();
                    if let Some(question) = pending
                        .params
                        .get("questions")
                        .and_then(Value::as_array)
                        .and_then(|questions| questions.first())
                        .and_then(|question| question.get("id"))
                        .and_then(Value::as_str)
                    {
                        answers.insert(question.to_owned(), json!({"answers": [answer]}));
                    }
                    Value::Object(answers)
                };
                json!({"answers": answers})
            }
            "mcpServer/elicitation/request" => json!({
                "action": if decision == PendingDecision::Accept { "accept" } else if decision == PendingDecision::Cancel { "cancel" } else { "decline" }
            }),
            _ => {
                return Err(format!(
                    "unsupported Codex app-server request `{}`",
                    pending.method
                ));
            }
        };
        self.send_response(pending.id, result)
    }

    fn account_read(&mut self, refresh_token: bool) -> Result<Value, String> {
        self.request("account/read", json!({"refreshToken": refresh_token}))
    }

    fn wait_for_login(
        &mut self,
        login_id: &str,
        cancel: Option<&AtomicBool>,
    ) -> Result<(), String> {
        let deadline = Instant::now() + LOGIN_TIMEOUT;
        loop {
            if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
                return Err("ChatGPT browser sign-in cancelled".to_owned());
            }
            let poll_deadline = std::cmp::min(deadline, Instant::now() + READ_POLL_INTERVAL);
            let message = match self.next_message(poll_deadline) {
                Ok(message) => message,
                Err(error) if error == APP_SERVER_TIMEOUT && Instant::now() < deadline => continue,
                Err(error) if error == APP_SERVER_TIMEOUT => {
                    return Err("timed out waiting for ChatGPT browser sign-in".to_owned());
                }
                Err(error) => return Err(error),
            };
            let Some(method) = message.get("method").and_then(Value::as_str) else {
                continue;
            };
            if method == "account/login/completed" {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                let matching = params
                    .get("loginId")
                    .and_then(Value::as_str)
                    .is_some_and(|reported| reported == login_id);
                if matching {
                    if params
                        .get("success")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        return Ok(());
                    }
                    return Err("ChatGPT browser sign-in was not completed successfully".to_owned());
                }
            }
            if let Some(id) = message.get("id") {
                self.send_error(id.clone(), -32601, "unsupported Codex app-server request")?;
            }
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = Value::from(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.write_message(json!({"id": id, "method": method, "params": params}))?;
        let deadline = Instant::now() + PROTOCOL_TIMEOUT;
        loop {
            let message = self.receive_incoming(deadline)?;
            if message.get("id") == Some(&id) {
                if let Some(error) = message.get("error") {
                    return Err(format!(
                        "Codex app-server request `{method}` failed: {}",
                        safe_error_text(error)
                    ));
                }
                return message.get("result").cloned().ok_or_else(|| {
                    format!("Codex app-server request `{method}` returned no result")
                });
            }
            if let Some(server_method) = message.get("method").and_then(Value::as_str) {
                if let Some(server_id) = message.get("id") {
                    if is_user_interaction_request(server_method) {
                        self.pending_request = Some(PendingServerRequest {
                            id: server_id.clone(),
                            method: server_method.to_owned(),
                            params: message.get("params").cloned().unwrap_or(Value::Null),
                        });
                    } else {
                        self.send_error(
                            server_id.clone(),
                            -32601,
                            "unsupported Codex app-server request",
                        )?;
                    }
                } else {
                    self.notifications.push_back(message);
                }
                continue;
            }
            return Err(format!(
                "Codex app-server returned an unexpected response while handling `{method}`"
            ));
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.write_message(json!({"method": method, "params": params}))
    }

    fn send_response(&mut self, id: Value, result: Value) -> Result<(), String> {
        self.write_message(json!({"id": id, "result": result}))
    }

    fn send_error(&mut self, id: Value, code: i64, message: &str) -> Result<(), String> {
        self.write_message(json!({
            "id": id,
            "error": {"code": code, "message": message}
        }))
    }

    fn write_message(&mut self, message: Value) -> Result<(), String> {
        serde_json::to_writer(&mut self.stdin, &message)
            .map_err(|error| format!("could not write to Codex app-server: {error}"))?;
        self.stdin
            .write_all(b"\n")
            .and_then(|_| self.stdin.flush())
            .map_err(|error| format!("could not flush Codex app-server request: {error}"))
    }

    fn next_message(&mut self, deadline: Instant) -> Result<Value, String> {
        if let Some(message) = self.notifications.pop_front() {
            return Ok(message);
        }
        self.receive_incoming(deadline)
    }

    fn receive_incoming(&self, deadline: Instant) -> Result<Value, String> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(APP_SERVER_TIMEOUT.to_owned());
        }
        match self.incoming.recv_timeout(remaining) {
            Ok(Ok(message)) => Ok(message),
            Ok(Err(error)) => Err(error),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(APP_SERVER_TIMEOUT.to_owned()),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err("Codex app-server closed its protocol stream".to_owned())
            }
        }
    }
}

pub fn discover_openai_oauth_models() -> Result<Vec<String>, String> {
    let mut server = CodexAppServer::connect()?;
    let account = server
        .account_read(false)
        .map_err(|error| format!("Codex account readiness check failed: {error}"))?;
    if account_type(&account) != Some("chatgpt") {
        return Err(
            "ChatGPT OAuth is not signed in to Codex; choose Sign in in the provider setup"
                .to_owned(),
        );
    }
    server.list_models()
}

pub fn start_openai_oauth_login() -> Result<OpenAiOAuthLogin, String> {
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let (sender, receiver) = mpsc::channel();
    let join = thread::Builder::new()
        .name("medusa-codex-oauth-login".to_owned())
        .spawn(move || {
            let result = (|| {
                let mut server = CodexAppServer::connect()?;
                server.ensure_authenticated_with_cancel(Some(&worker_cancel))?;
                server.list_models()
            })();
            if !worker_cancel.load(Ordering::SeqCst) {
                let _ = sender.send(result);
            }
        })
        .map_err(|error| format!("could not start Codex browser sign-in: {error}"))?;
    Ok(OpenAiOAuthLogin {
        receiver,
        cancel,
        join: Some(join),
    })
}

pub fn ensure_openai_oauth_connected() -> Result<Vec<String>, String> {
    let mut server = CodexAppServer::connect()?;
    server.ensure_authenticated()?;
    server.list_models()
}

pub fn codex_program() -> &'static str {
    if cfg!(windows) { "codex.cmd" } else { "codex" }
}

fn codex_app_server_args() -> Vec<String> {
    vec![
        "app-server".to_owned(),
        "--stdio".to_owned(),
        "-c".to_owned(),
        format!("openai_base_url={DIRECT_CHATGPT_BASE_URL:?}"),
    ]
}

fn read_protocol_lines(stdout: impl io::Read, sender: mpsc::Sender<Result<Value, String>>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                let _ = sender.send(Err("Codex app-server closed its protocol stream".to_owned()));
                return;
            }
            Ok(length) if length > MAX_LINE_BYTES => {
                let _ = sender.send(Err(format!(
                    "Codex app-server protocol line exceeded {MAX_LINE_BYTES} bytes"
                )));
                return;
            }
            Ok(_) => match serde_json::from_str::<Value>(&line) {
                Ok(message) if message.is_object() => {
                    if sender.send(Ok(message)).is_err() {
                        return;
                    }
                }
                Ok(_) => {
                    let _ = sender.send(Err(
                        "Codex app-server returned a non-object message".to_owned()
                    ));
                    return;
                }
                Err(error) => {
                    let _ = sender.send(Err(format!(
                        "Codex app-server returned invalid JSON: {error}"
                    )));
                    return;
                }
            },
            Err(error) => {
                let _ = sender.send(Err(format!(
                    "could not read Codex app-server output: {error}"
                )));
                return;
            }
        }
    }
}

fn parse_models(value: Value) -> Result<Vec<String>, String> {
    let mut models = value
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            item.get("id")
                .or_else(|| item.get("slug"))
                .or_else(|| item.get("model"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    models.sort();
    models.dedup();
    if models.is_empty() {
        Err("Codex app-server returned no selectable ChatGPT models".to_owned())
    } else {
        Ok(models)
    }
}

fn account_type(value: &Value) -> Option<&str> {
    value
        .get("account")
        .and_then(|account| account.get("type"))
        .and_then(Value::as_str)
}

fn extract_agent_text(turn: &Value) -> String {
    turn.get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("agentMessage"))
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("")
}

fn is_user_interaction_request(method: &str) -> bool {
    matches!(
        method,
        "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "item/permissions/requestApproval"
            | "item/tool/requestUserInput"
            | "mcpServer/elicitation/request"
    )
}

fn safe_error_text(value: &Value) -> String {
    let text = value
        .as_str()
        .map(str::to_owned)
        .or_else(|| {
            value
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "protocol error".to_owned());
    text.replace("Bearer ", "Bearer [redacted] ")
        .replace("access_token", "[redacted-token]")
        .chars()
        .take(512)
        .collect()
}

fn open_browser_url(url: &str) -> Result<(), String> {
    let (program, args) = browser_open_spec(url);
    hidden_command(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("could not open the ChatGPT sign-in page: {error}"))
}

fn browser_open_spec(url: &str) -> (&'static str, Vec<String>) {
    if cfg!(windows) {
        (
            "rundll32.exe",
            vec!["url.dll,FileProtocolHandler".to_owned(), url.to_owned()],
        )
    } else if cfg!(target_os = "macos") {
        ("open", vec![url.to_owned()])
    } else {
        ("xdg-open", vec![url.to_owned()])
    }
}

fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovered_models_are_sorted_deduplicated_and_accept_slugs() {
        assert_eq!(
            parse_models(json!({
                "data": [{"id": "gpt-b"}, {"slug": "gpt-a"}, {"model": "gpt-a"}]
            }))
            .expect("models"),
            vec!["gpt-a".to_owned(), "gpt-b".to_owned()]
        );
        assert!(parse_models(json!({"data": []})).is_err());
    }

    #[test]
    fn account_type_requires_chatgpt_account_shape() {
        assert_eq!(
            account_type(&json!({"account": {"type": "chatgpt"}})),
            Some("chatgpt")
        );
        assert_eq!(account_type(&json!({"account": null})), None);
    }

    #[test]
    fn turn_text_is_extracted_only_from_agent_messages() {
        assert_eq!(
            extract_agent_text(&json!({
                "items": [
                    {"type": "userMessage", "text": "ignore"},
                    {"type": "agentMessage", "text": "hello"},
                    {"type": "agentMessage", "text": " world"}
                ]
            })),
            "hello world"
        );
    }

    #[test]
    fn browser_launcher_keeps_oauth_url_out_of_shell_parsing() {
        let url = "https://auth.openai.com/oauth/authorize?response_type=code&client_id=test";
        let (program, args) = browser_open_spec(url);
        assert_eq!(args.last().map(String::as_str), Some(url));
        #[cfg(windows)]
        {
            assert_eq!(program, "rundll32.exe");
            assert_eq!(
                args.first().map(String::as_str),
                Some("url.dll,FileProtocolHandler")
            );
        }
        #[cfg(target_os = "macos")]
        assert_eq!(program, "open");
        #[cfg(all(not(windows), not(target_os = "macos")))]
        assert_eq!(program, "xdg-open");
    }

    #[test]
    fn codex_executable_matches_platform_launcher() {
        #[cfg(windows)]
        assert_eq!(codex_program(), "codex.cmd");
        #[cfg(not(windows))]
        assert_eq!(codex_program(), "codex");
    }

    #[test]
    fn app_server_launch_args_force_the_direct_chatgpt_endpoint() {
        let args = codex_app_server_args();
        assert_eq!(args.first().map(String::as_str), Some("app-server"));
        assert_eq!(args.get(1).map(String::as_str), Some("--stdio"));
        assert_eq!(args.get(2).map(String::as_str), Some("-c"));
        assert_eq!(
            args.get(3).map(String::as_str),
            Some("openai_base_url=\"https://chatgpt.com/backend-api/codex\"")
        );
    }
}
