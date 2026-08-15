mod browser;
mod browser_dispatch;
mod compound;
mod executable_skills;
mod filesystem;
mod git;
mod intelligence;
mod shell;
pub(crate) mod skills;
mod web;
mod pipeline {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/tool_pipeline.rs"));
}

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use medusa_browser_client::{BrowserClient, BrowserRequest, BrowserResponse};
use medusa_capabilities::{CapabilityRegistry, SystemProbe};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_extensions::DesktopCommanderSettings;
use medusa_provider::ToolDefinition;
use serde_json::Value;
#[cfg(test)]
use serde_json::json;

use pipeline::{GuardDecision, ToolExecutionPipeline, ToolPipelineRequest};

const BROWSER_VERIFY_URL_ENV: &str = "MEDUSA_BROWSER_VERIFY_URL";
const BROWSER_VERIFICATION_ORIGIN_ENV: &str = "MEDUSA_BROWSER_VERIFICATION_ORIGIN";
const MAX_BROWSER_ARTIFACT_BYTES: usize = 8 * 1024 * 1024;

type BrowserSessionKey = (PathBuf, String);
type SharedBrowserSession = Arc<Mutex<BrowserToolSession>>;

enum BrowserIdleCommand {
    Busy,
    Reset(Duration),
    Stop,
}

struct BrowserToolSession {
    client: BrowserClient,
    initialized: bool,
    idle_tx: mpsc::Sender<BrowserIdleCommand>,
}

static BROWSER_SESSIONS: OnceLock<Mutex<BTreeMap<BrowserSessionKey, SharedBrowserSession>>> =
    OnceLock::new();
static BROWSER_NEVER_CANCELLED: AtomicBool = AtomicBool::new(false);

/// Single policy-aware registry for built-in tools shared by every agent frontend.
#[derive(Clone, Debug)]
pub struct ToolManager {
    desktop_commander: DesktopCommanderSettings,
}

impl ToolManager {
    #[must_use]
    pub fn new(desktop_commander: DesktopCommanderSettings) -> Self {
        Self { desktop_commander }
    }

    /// Compatibility projection for callers without a repository context. Discovery failure
    /// fails closed by returning no model tools.
    #[must_use]
    pub fn definitions(&self, read_only: bool) -> Vec<ToolDefinition> {
        CapabilityRegistry::discover_with_desktop(
            PathBuf::from("."),
            &SystemProbe,
            self.desktop_commander.clone(),
        )
        .map_or_else(|_| Vec::new(), |registry| registry.model_tools(read_only))
    }

    pub fn definitions_for(
        &self,
        repo: &Path,
        read_only: bool,
    ) -> MedusaResult<Vec<ToolDefinition>> {
        built_in_tools(repo, &self.desktop_commander, read_only)
    }

    pub fn execute(&self, repo: &Path, name: &str, input: &Value) -> MedusaResult<String> {
        execute_tool_cancellable(repo, name, input, &BROWSER_NEVER_CANCELLED)
    }

    pub fn execute_approved(&self, repo: &Path, name: &str, input: &Value) -> MedusaResult<String> {
        execute_approved_tool_cancellable(repo, name, input, &BROWSER_NEVER_CANCELLED)
    }
}

pub(crate) fn available_skills(repo: &Path) -> Vec<skills::SkillSummary> {
    skills::summaries(repo)
}

pub(crate) fn built_in_tools(
    repo: &Path,
    desktop_commander: &DesktopCommanderSettings,
    read_only: bool,
) -> MedusaResult<Vec<ToolDefinition>> {
    CapabilityRegistry::discover_with_desktop(
        repo.to_path_buf(),
        &SystemProbe,
        desktop_commander.clone(),
    )
    .map(|registry| registry.model_tools(read_only))
}

pub(crate) fn execute_tool(repo: &Path, name: &str, input: &Value) -> MedusaResult<String> {
    if is_model_browser_tool(name) {
        return execute_browser_tool(repo, name, input, &BROWSER_NEVER_CANCELLED);
    }
    match name {
        "fs_read" => filesystem::read(repo, input_string(input, "path")?),
        "fs_create_dir" => filesystem::create_dir(repo, input_string(input, "path")?),
        "fs_write" => filesystem::write(
            repo,
            input_string(input, "path")?,
            input_string(input, "content")?,
        ),
        "search_text" => filesystem::search(repo, input_string(input, "query")?),
        "semantic_capabilities" => intelligence::semantic_capabilities(),
        "code_index" => intelligence::code_index(repo, input),
        "inspect_target" => compound::inspect_target(repo, input),
        "apply_structured_patch" => compound::apply_structured_patch(repo, input),
        "verify_impacted" => compound::verify_impacted(repo, input),
        "typescript_semantic" => intelligence::typescript_semantic(repo, input),
        "patch_apply" => intelligence::patch_apply(repo, input),
        "symbol_rename" => intelligence::symbol_rename(repo, input),
        "shell_run" => {
            let program = input_string(input, "program")?;
            let args = input
                .get("args")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid_tool("args must be an array"))?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| invalid_tool("every arg must be a string"))
                })
                .collect::<MedusaResult<Vec<_>>>()?;
            let output_mode = crate::output_envelope::OutputMode::parse(input_optional_string(
                input,
                "output_mode",
            )?)?;
            shell::run(repo, program, &args, output_mode)
        }
        "web_search" => web::search(
            input_string(input, "query")?,
            input_domains(input, "allowed_domains")?,
            input_domains(input, "blocked_domains")?,
        ),
        "web_fetch" => web::fetch(
            input_string(input, "url")?,
            input.get("prompt").and_then(Value::as_str),
        ),
        "skill_read" => skills::read(
            repo,
            input_string(input, "name")?,
            input.get("scope").and_then(Value::as_str),
        ),
        "skill_execute" => {
            let cancellation = AtomicBool::new(false);
            executable_skills::run(repo, input, &cancellation)
        }
        "git_checkpoint" => git::checkpoint(repo, input_string(input, "message")?),
        _ => Err(invalid_tool(format!("unknown tool: {name}"))),
    }
}

fn is_model_browser_tool(name: &str) -> bool {
    matches!(
        name,
        "browser_navigate"
            | "browser_snapshot"
            | "browser_click"
            | "browser_fill"
            | "browser_press"
            | "browser_screenshot"
            | "browser_tabs"
            | "browser_close"
            | "browser_ping"
    )
}

fn browser_sessions() -> &'static Mutex<BTreeMap<BrowserSessionKey, SharedBrowserSession>> {
    BROWSER_SESSIONS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn browser_policy_denied(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::PolicyDenied, ErrorCategory::Policy, message)
}

fn browser_dependency_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Environment,
        message,
    )
}

fn browser_verification_route() -> MedusaResult<String> {
    std::env::var(BROWSER_VERIFY_URL_ENV)
        .ok()
        .map(|route| route.trim().to_owned())
        .filter(|route| !route.is_empty())
        .ok_or_else(|| {
            browser_policy_denied(
                "model browser actions require a Medusa-owned MEDUSA_BROWSER_VERIFY_URL route",
            )
        })
}

fn validate_browser_model_input(name: &str, input: &Value) -> MedusaResult<()> {
    let object = input
        .as_object()
        .ok_or_else(|| invalid_tool("browser tool input must be an object"))?;
    for reserved in ["verified", "verification", "trusted", "trust", "authority"] {
        if object.contains_key(reserved) {
            return Err(browser_policy_denied(
                "browser verification authority is Medusa-owned and cannot be supplied by the model",
            ));
        }
    }
    let allowed: &[&str] = match name {
        "browser_navigate" | "browser_snapshot" | "browser_tabs" | "browser_close"
        | "browser_ping" => &[],
        "browser_click" => &["ref", "selector"],
        "browser_fill" => &["ref", "selector", "value"],
        "browser_press" => &["key"],
        "browser_screenshot" => &["full_page"],
        _ => return Err(invalid_tool(format!("unknown model browser tool: {name}"))),
    };
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid_tool(format!(
            "browser tool {name} does not accept field `{key}`"
        )));
    }
    Ok(())
}

fn browser_session_key(repo: &Path, route: &str) -> BrowserSessionKey {
    (
        fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf()),
        route.to_owned(),
    )
}

fn browser_envelope_config(
    repo: &Path,
    runtime: &medusa_config::MedusaConfig,
) -> crate::output_envelope::EnvelopeConfig {
    crate::output_envelope::EnvelopeConfig {
        head_bytes: runtime.envelope.head_bytes,
        tail_bytes: runtime.envelope.tail_bytes,
        max_artifact_bytes: runtime
            .daemon_max_artifact_bytes
            .min(MAX_BROWSER_ARTIFACT_BYTES),
        session_root: repo.join(".medusa"),
    }
}

fn create_browser_session(
    runtime: &medusa_config::MedusaConfig,
    route: &str,
    key: &BrowserSessionKey,
) -> MedusaResult<SharedBrowserSession> {
    if !runtime.browser.enabled {
        return Err(browser_policy_denied(
            "model browser actions are quarantined until MEDUSA_BROWSER_ENABLED is explicitly enabled",
        ));
    }
    let path = runtime.browser.path.as_ref().ok_or_else(|| {
        browser_policy_denied(
            "model browser actions require an explicit MEDUSA_BROWSER_PATH sidecar",
        )
    })?;
    if !path.is_file() {
        return Err(browser_dependency_error(format!(
            "configured browser sidecar does not exist: {}",
            path.display()
        )));
    }
    let command = path
        .to_str()
        .ok_or_else(|| invalid_tool("browser sidecar path must be UTF-8"))?;
    let client =
        BrowserClient::spawn_with_env(command, &[(BROWSER_VERIFICATION_ORIGIN_ENV, route)])?;
    let (idle_tx, idle_rx) = mpsc::channel();
    let session = Arc::new(Mutex::new(BrowserToolSession {
        client,
        initialized: false,
        idle_tx,
    }));
    let weak_session = Arc::downgrade(&session);
    let idle_key = key.clone();
    thread::Builder::new()
        .name("medusa-browser-idle-lifecycle".to_owned())
        .spawn(move || browser_idle_worker(idle_key, weak_session, idle_rx))
        .map_err(|error| {
            browser_dependency_error(format!("could not start browser idle lifecycle: {error}"))
        })?;
    Ok(session)
}

fn get_browser_session(
    repo: &Path,
    route: &str,
    runtime: &medusa_config::MedusaConfig,
) -> MedusaResult<(BrowserSessionKey, SharedBrowserSession)> {
    let key = browser_session_key(repo, route);
    let mut sessions = browser_sessions().lock().map_err(|_| {
        MedusaError::new(
            ErrorCode::InternalInvariant,
            ErrorCategory::Internal,
            "browser session registry lock was poisoned",
        )
    })?;
    let session = if let Some(session) = sessions.get(&key) {
        Arc::clone(session)
    } else {
        let session = create_browser_session(runtime, route, &key)?;
        sessions.insert(key.clone(), Arc::clone(&session));
        session
    };
    Ok((key, session))
}

fn ensure_browser_initialized(
    session: &mut BrowserToolSession,
    route: &str,
    timeout: Duration,
    cancellation: &AtomicBool,
) -> MedusaResult<()> {
    if session.initialized {
        return Ok(());
    }
    match session.client.request_with_control(
        BrowserRequest::Navigate {
            url: route.to_owned(),
        },
        timeout,
        cancellation,
    )? {
        BrowserResponse::Navigate { status, .. } if status < 400 => {
            session.initialized = true;
            Ok(())
        }
        BrowserResponse::Navigate { status, .. } => Err(MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            format!("browser verification route returned HTTP {status}"),
        )),
        BrowserResponse::Error { code, message } => Err(MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            format!("browser verification route failed: {code}: {message}"),
        )),
        other => Err(MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            format!("unexpected browser navigation response: {other:?}"),
        )),
    }
}

fn remove_browser_session(key: &BrowserSessionKey, expected: &SharedBrowserSession) {
    let Ok(mut sessions) = browser_sessions().lock() else {
        return;
    };
    if sessions
        .get(key)
        .is_some_and(|current| Arc::ptr_eq(current, expected))
    {
        sessions.remove(key);
    }
}

fn browser_idle_worker(
    key: BrowserSessionKey,
    session: std::sync::Weak<Mutex<BrowserToolSession>>,
    receiver: mpsc::Receiver<BrowserIdleCommand>,
) {
    let mut idle_timeout = None;
    loop {
        let command = match idle_timeout {
            None => match receiver.recv() {
                Ok(command) => command,
                Err(_) => break,
            },
            Some(timeout) => match receiver.recv_timeout(timeout) {
                Ok(command) => command,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Some(session) = session.upgrade() {
                        remove_browser_session(&key, &session);
                    }
                    break;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            },
        };
        match command {
            BrowserIdleCommand::Busy => idle_timeout = None,
            BrowserIdleCommand::Reset(timeout) => idle_timeout = Some(timeout),
            BrowserIdleCommand::Stop => break,
        }
    }
}

fn execute_browser_tool(
    repo: &Path,
    name: &str,
    input: &Value,
    cancellation: &AtomicBool,
) -> MedusaResult<String> {
    validate_browser_model_input(name, input)?;
    let route = browser_verification_route()?;
    let runtime = medusa_config::MedusaConfig::from_env();
    if !runtime.browser.enabled || runtime.browser.path.is_none() {
        return Err(browser_policy_denied(
            "model browser actions remain quarantined until the verified browser sidecar is explicitly configured",
        ));
    }
    let timeout = Duration::from_millis(runtime.browser.timeout_ms.max(1));
    let key = browser_session_key(repo, &route);
    if name == "browser_close" {
        let session = browser_sessions()
            .lock()
            .ok()
            .and_then(|mut sessions| sessions.remove(&key));
        if let Some(session) = session {
            let mut state = session.lock().map_err(|_| {
                MedusaError::new(
                    ErrorCode::InternalInvariant,
                    ErrorCategory::Internal,
                    "browser session lock was poisoned",
                )
            })?;
            let _ = state.idle_tx.send(BrowserIdleCommand::Stop);
            state
                .client
                .request_with_control(BrowserRequest::Close, timeout, cancellation)?;
        }
        return Ok("browser verification session closed".to_owned());
    }

    let (key, session) = get_browser_session(repo, &route, &runtime)?;
    let envelope_config = browser_envelope_config(repo, &runtime);
    let result = {
        let mut state = session.lock().map_err(|_| {
            MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                "browser session lock was poisoned",
            )
        })?;
        state
            .idle_tx
            .send(BrowserIdleCommand::Busy)
            .map_err(|_| browser_dependency_error("browser idle lifecycle stopped unexpectedly"))?;
        let result = (|| {
            ensure_browser_initialized(&mut state, &route, timeout, cancellation)?;
            let effective_input = if name == "browser_navigate" {
                serde_json::json!({"url": route})
            } else {
                input.clone()
            };
            browser::run(
                repo,
                &mut state.client,
                &envelope_config,
                name,
                &effective_input,
                timeout,
                cancellation,
            )
        })();
        if result.is_ok() {
            if state
                .idle_tx
                .send(BrowserIdleCommand::Reset(timeout))
                .is_err()
            {
                return Err(browser_dependency_error(
                    "browser idle lifecycle stopped unexpectedly",
                ));
            }
        } else {
            let _ = state.idle_tx.send(BrowserIdleCommand::Stop);
        }
        result
    };
    if result.is_err() {
        remove_browser_session(&key, &session);
    }
    result
}

fn capability_guard(repo: &Path, name: &str) -> GuardDecision {
    let registry = match CapabilityRegistry::discover(repo) {
        Ok(registry) => registry,
        Err(error) => return GuardDecision::Deny(format!("capability discovery failed: {error}")),
    };
    let id = format!("tool.{name}");
    let Some(entry) = registry.entry(&id) else {
        return GuardDecision::Deny(format!("tool is not registered: {name}"));
    };
    if !entry.projected_to(medusa_capabilities::CapabilitySurface::Model) {
        return GuardDecision::Deny(format!(
            "tool is unavailable: {name}: {}",
            entry.readiness.detail
        ));
    }
    GuardDecision::Allow
}

fn execute_tool_cancellable_with_context_unchecked(
    repo: &Path,
    name: &str,
    input: &Value,
    cancellation: &AtomicBool,
    mutation_context: Option<&crate::transaction::MutationContext>,
) -> MedusaResult<String> {
    if cancellation.load(Ordering::Acquire) {
        return Err(cancelled_tool(name));
    }
    if name == "fs_write" {
        if let Some(context) = mutation_context {
            let relative = input_string(input, "path")?;
            let content = input_string(input, "content")?;
            let outcome = filesystem::write_with_context(repo, relative, content, context)?;
            return Ok(format!(
                "wrote {} bytes to {relative}; mutation_ids={}",
                content.len(),
                outcome.mutation_ids.join(",")
            ));
        }
    }
    if name == "skill_execute" {
        return executable_skills::run(repo, input, cancellation);
    }
    if is_model_browser_tool(name) {
        return execute_browser_tool(repo, name, input, cancellation);
    }
    if name != "shell_run" {
        return execute_tool(repo, name, input);
    }
    let program = input_string(input, "program")?;
    let args = input
        .get("args")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_tool("args must be an array"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid_tool("every arg must be a string"))
        })
        .collect::<MedusaResult<Vec<_>>>()?;
    let output_mode =
        crate::output_envelope::OutputMode::parse(input_optional_string(input, "output_mode")?)?;
    shell::run_cancellable(repo, program, &args, output_mode, cancellation)
}

pub(crate) fn execute_tool_cancellable_with_context(
    repo: &Path,
    name: &str,
    input: &Value,
    cancellation: &AtomicBool,
    mutation_context: Option<&crate::transaction::MutationContext>,
) -> MedusaResult<String> {
    let repo_owned = repo.to_path_buf();
    let name_owned = name.to_owned();
    let pipeline = ToolExecutionPipeline::new().with_guard("capability_readiness", move |_| {
        capability_guard(&repo_owned, &name_owned)
    });
    pipeline
        .execute(
            ToolPipelineRequest::built_in(name, input),
            cancellation,
            |canonical_input| {
                execute_tool_cancellable_with_context_unchecked(
                    repo,
                    name,
                    canonical_input,
                    cancellation,
                    mutation_context,
                )
            },
        )
        .into_result()
}

pub(crate) fn execute_tool_cancellable(
    repo: &Path,
    name: &str,
    input: &Value,
    cancellation: &AtomicBool,
) -> MedusaResult<String> {
    execute_tool_cancellable_with_context(repo, name, input, cancellation, None)
}

fn execute_approved_tool_cancellable_unchecked(
    repo: &Path,
    name: &str,
    input: &Value,
    cancellation: &AtomicBool,
) -> MedusaResult<String> {
    if cancellation.load(Ordering::Acquire) {
        return Err(cancelled_tool(name));
    }
    if name != "shell_run" {
        return execute_approved_tool(repo, name, input);
    }
    let program = input_string(input, "program")?;
    let args = input
        .get("args")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_tool("args must be an array"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid_tool("every arg must be a string"))
        })
        .collect::<MedusaResult<Vec<_>>>()?;
    let output_mode =
        crate::output_envelope::OutputMode::parse(input_optional_string(input, "output_mode")?)?;
    shell::run_approved_cancellable(repo, program, &args, output_mode, cancellation)
}

pub(crate) fn execute_approved_tool_cancellable(
    repo: &Path,
    name: &str,
    input: &Value,
    cancellation: &AtomicBool,
) -> MedusaResult<String> {
    let repo_owned = repo.to_path_buf();
    let name_owned = name.to_owned();
    let pipeline = ToolExecutionPipeline::new().with_guard("capability_readiness", move |_| {
        capability_guard(&repo_owned, &name_owned)
    });
    let mut request = ToolPipelineRequest::built_in(name, input);
    request.approval_required = true;
    request.approval_granted = true;
    pipeline
        .execute(request, cancellation, |canonical_input| {
            execute_approved_tool_cancellable_unchecked(
                repo,
                name,
                canonical_input,
                cancellation,
            )
        })
        .into_result()
}

pub(crate) fn execute_approved_tool(
    repo: &Path,
    name: &str,
    input: &Value,
) -> MedusaResult<String> {
    match name {
        "fs_create_dir" => filesystem::create_dir_approved(input_string(input, "path")?),
        "fs_write" => filesystem::write_approved(
            input_string(input, "path")?,
            input_string(input, "content")?,
        ),
        "shell_run" => {
            let program = input_string(input, "program")?;
            let args = input
                .get("args")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid_tool("args must be an array"))?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| invalid_tool("every arg must be a string"))
                })
                .collect::<MedusaResult<Vec<_>>>()?;
            let output_mode = crate::output_envelope::OutputMode::parse(input_optional_string(
                input,
                "output_mode",
            )?)?;
            shell::run_approved(repo, program, &args, output_mode)
        }
        _ => Err(MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            format!("{name} cannot be authorized interactively"),
        )),
    }
}

fn input_domains(input: &Value, key: &str) -> MedusaResult<Vec<String>> {
    let Some(domains) = input.get(key) else {
        return Ok(Vec::new());
    };
    domains
        .as_array()
        .ok_or_else(|| invalid_tool(format!("{key} must be an array")))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(|domain| domain.trim().to_ascii_lowercase())
                .filter(|domain| !domain.is_empty())
                .ok_or_else(|| {
                    invalid_tool(format!("every {key} entry must be a non-empty string"))
                })
        })
        .collect()
}

pub(crate) fn input_string<'a>(input: &'a Value, key: &str) -> MedusaResult<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_tool(format!("{key} must be a string")))
}

fn input_optional_string<'a>(input: &'a Value, key: &str) -> MedusaResult<Option<&'a str>> {
    match input.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| invalid_tool(format!("{key} must be a string"))),
    }
}

pub(crate) fn input_usize(input: &Value, key: &str) -> MedusaResult<usize> {
    input
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| invalid_tool(format!("{key} must be a non-negative integer")))
}

pub fn format_command_output(
    program: &str,
    args: &[impl AsRef<str>],
    stdout: &[u8],
    stderr: &[u8],
) -> Vec<String> {
    vec![
        format!(
            "command={} {}",
            program,
            args.iter()
                .map(|arg| arg.as_ref())
                .collect::<Vec<_>>()
                .join(" ")
        ),
        format!("stdout={}", String::from_utf8_lossy(stdout)),
        format!("stderr={}", String::from_utf8_lossy(stderr)),
    ]
}

fn cancelled_tool(name: &str) -> MedusaError {
    let mut error = MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Execution,
        format!("tool execution cancelled: {name}"),
    );
    error
        .context
        .insert("cancelled".into(), serde_json::Value::Bool(true));
    error
}

pub(crate) fn invalid_tool(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_string_rejects_present_non_string_values() {
        assert_eq!(
            input_optional_string(&json!({}), "output_mode").expect("absent mode"),
            None
        );

        let normal = json!({"output_mode": "normal"});
        assert_eq!(
            input_optional_string(&normal, "output_mode").expect("string mode"),
            Some("normal")
        );

        for invalid in [Value::Null, json!(42), json!({"mode": "compact"})] {
            let input = json!({"output_mode": invalid});
            let error = input_optional_string(&input, "output_mode")
                .expect_err("present non-string mode must fail closed");
            assert!(error.to_string().contains("output_mode must be a string"));
        }
    }

    #[test]
    fn browser_model_tools_are_explicit_and_evaluate_stays_internal() {
        for name in [
            "browser_navigate",
            "browser_snapshot",
            "browser_click",
            "browser_fill",
            "browser_press",
            "browser_screenshot",
            "browser_tabs",
            "browser_close",
            "browser_ping",
        ] {
            assert!(is_model_browser_tool(name), "{name}");
        }
        assert!(!is_model_browser_tool("browser_evaluate"));
        assert!(!is_model_browser_tool("fs_read"));
    }

    #[test]
    fn model_cannot_supply_browser_verification_authority() {
        for input in [
            json!({"verified": true}),
            json!({"verification": true}),
            json!({"trusted": true}),
            json!({"trust": true}),
            json!({"authority": true}),
        ] {
            let error = validate_browser_model_input("browser_snapshot", &input)
                .expect_err("model trust metadata must fail closed");
            assert_eq!(error.code, ErrorCode::PolicyDenied);
        }
    }

    #[test]
    fn browser_navigation_accepts_no_model_selected_url() {
        validate_browser_model_input("browser_navigate", &json!({}))
            .expect("empty navigation input");
        let error =
            validate_browser_model_input("browser_navigate", &json!({"url": "http://127.0.0.1:9"}))
                .expect_err("model-selected URL must be rejected");
        assert!(error.to_string().contains("does not accept field `url`"));
    }
}
