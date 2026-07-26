use std::{
    env, fs,
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    process::Command,
    time::Duration,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use reqwest::{StatusCode, blocking::Client};
use serde::Deserialize;
use serde_json::{Value, json};

const DEFAULT_GATEWAY: &str = "http://127.0.0.1:10531/v1";
const PREFLIGHT_ENV: &str = "MEDUSA_OAUTH_PREFLIGHT";

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ProviderProfile {
    connection: String,
    model: String,
    base_url: Option<String>,
    configured: bool,
}

#[derive(Debug, PartialEq)]
struct PreflightReport {
    model: String,
    tool_calling: &'static str,
    streaming: &'static str,
    cancellation: &'static str,
}

pub(crate) fn run_if_needed(args: &[String]) -> MedusaResult<()> {
    if !is_coding_session(args) {
        return Ok(());
    }
    let Some(profile) = load_profile()? else {
        return Ok(());
    };
    if !profile.configured || profile.connection != "chatgpt-oauth" {
        return Ok(());
    }
    if preflight_disabled() {
        eprintln!(
            "ChatGPT OAuth gateway preflight is disabled by {PREFLIGHT_ENV}; model, tool-calling, streaming, and cancellation compatibility are unverified."
        );
        return Ok(());
    }

    ensure_gateway_running()?;
    let base_url = profile.base_url.as_deref().unwrap_or(DEFAULT_GATEWAY);
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(gateway_transport_error)?;
    let report = probe_gateway(&client, base_url, &profile.model)?;
    eprintln!(
        "ChatGPT OAuth gateway verified: model={}, tool_calling={}, streaming={}, cancellation={}.",
        report.model, report.tool_calling, report.streaming, report.cancellation
    );
    Ok(())
}

fn is_coding_session(args: &[String]) -> bool {
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            return false;
        }
        if matches!(arg.as_str(), "--repo" | "--set" | "--prompt" | "--resume") {
            index += 2;
            continue;
        }
        if arg.starts_with("--repo=") || arg.starts_with("--set=") {
            index += 1;
            continue;
        }
        if arg.starts_with('-') {
            index += 1;
            continue;
        }
        return matches!(arg.as_str(), "run" | "resume");
    }
    true
}

fn load_profile() -> MedusaResult<Option<ProviderProfile>> {
    let path = profile_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|error| {
        preflight_error(
            ErrorCategory::Environment,
            format!("read OAuth provider profile {}: {error}", path.display()),
        )
    })?;
    let profile = toml::from_str(&text).map_err(|error| {
        preflight_error(
            ErrorCategory::Validation,
            format!("parse OAuth provider profile {}: {error}", path.display()),
        )
    })?;
    Ok(Some(profile))
}

fn profile_path() -> MedusaResult<PathBuf> {
    let base = if cfg!(windows) {
        env::var_os("APPDATA").map(PathBuf::from)
    } else if let Some(path) = env::var_os("XDG_CONFIG_HOME") {
        Some(PathBuf::from(path))
    } else {
        env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
    }
    .ok_or_else(|| {
        preflight_error(
            ErrorCategory::Environment,
            "could not resolve the user configuration directory for OAuth preflight",
        )
    })?;
    Ok(base.join("medusa").join("provider.toml"))
}

fn preflight_disabled() -> bool {
    env::var(PREFLIGHT_ENV).ok().is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "skip" | "disabled"
        )
    })
}

fn ensure_gateway_running() -> MedusaResult<()> {
    let address: SocketAddr = "127.0.0.1:10531".parse().map_err(|error| {
        preflight_error(
            ErrorCategory::Validation,
            format!("invalid OAuth gateway address: {error}"),
        )
    })?;
    if TcpStream::connect_timeout(&address, Duration::from_millis(300)).is_ok() {
        return Ok(());
    }
    let status = Command::new("npx")
        .args(["--yes", "openai-oauth@latest", "--detach"])
        .status()
        .map_err(|error| {
            preflight_error(
                ErrorCategory::Environment,
                format!(
                    "OAuth gateway is unavailable and openai-oauth could not be started with npx: {error}"
                ),
            )
        })?;
    if !status.success() {
        return Err(preflight_error(
            ErrorCategory::Environment,
            format!(
                "OAuth gateway startup failed with {status}; run `npx openai-oauth@latest login` and retry"
            ),
        ));
    }
    if TcpStream::connect_timeout(&address, Duration::from_secs(2)).is_err() {
        return Err(preflight_error(
            ErrorCategory::Environment,
            "OAuth gateway process started but 127.0.0.1:10531 is unreachable",
        ));
    }
    Ok(())
}

fn probe_gateway(client: &Client, base_url: &str, model: &str) -> MedusaResult<PreflightReport> {
    let base_url = base_url.trim_end_matches('/');
    let models = client
        .get(format!("{base_url}/models"))
        .send()
        .map_err(gateway_transport_error)?;
    let models = require_success(models, "gateway model discovery")?;
    verify_model(&models, model)?;

    let request = json!({
        "model": model,
        "messages": [{"role": "user", "content": "Call medusa_preflight with an empty object and no prose."}],
        "tools": [{
            "type": "function",
            "function": {
                "name": "medusa_preflight",
                "description": "Medusa OAuth gateway compatibility probe",
                "parameters": {"type": "object", "properties": {}, "additionalProperties": false}
            }
        }],
        "tool_choice": {"type": "function", "function": {"name": "medusa_preflight"}},
        "max_tokens": 16,
        "temperature": 0,
        "stream": false
    });
    let tool_response = client
        .post(format!("{base_url}/chat/completions"))
        .json(&request)
        .send()
        .map_err(gateway_transport_error)?;
    let tool_response = require_success(tool_response, "gateway tool-call probe")?;
    verify_tool_call(&tool_response)?;

    let mut streaming_request = request;
    streaming_request["stream"] = Value::Bool(true);
    let streaming_response = client
        .post(format!("{base_url}/chat/completions"))
        .json(&streaming_request)
        .send()
        .map_err(gateway_transport_error)?;
    let streaming_response = require_success(streaming_response, "gateway streaming probe")?;
    verify_stream(&streaming_response)?;

    Ok(PreflightReport {
        model: model.to_owned(),
        tool_calling: "verified",
        streaming: "verified",
        cancellation: "unverified (gateway exposes no portable cancellation capability endpoint)",
    })
}

fn require_success(response: reqwest::blocking::Response, operation: &str) -> MedusaResult<String> {
    let status = response.status();
    let body = response.text().unwrap_or_default();
    if status.is_success() {
        return Ok(body);
    }
    let (category, class) = if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN
    {
        (ErrorCategory::Policy, "authentication")
    } else if status == StatusCode::NOT_FOUND {
        (ErrorCategory::Validation, "gateway endpoint")
    } else if status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY {
        (ErrorCategory::Validation, "protocol")
    } else {
        (ErrorCategory::Transient, "gateway")
    };
    Err(preflight_error(
        category,
        format!("{operation} failed: incompatible {class}, HTTP {status}: {body}"),
    ))
}

fn verify_model(body: &str, model: &str) -> MedusaResult<()> {
    let value: Value = serde_json::from_str(body).map_err(|error| {
        protocol_error(format!("model discovery returned malformed JSON: {error}"))
    })?;
    let available = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_error("model discovery response has no data array"))?;
    if available
        .iter()
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .any(|id| id == model)
    {
        Ok(())
    } else {
        Err(preflight_error(
            ErrorCategory::Validation,
            format!("configured OAuth model `{model}` is not exposed by the gateway"),
        ))
    }
}

fn verify_tool_call(body: &str) -> MedusaResult<()> {
    let value: Value = serde_json::from_str(body)
        .map_err(|error| protocol_error(format!("tool probe returned malformed JSON: {error}")))?;
    let calls = value
        .pointer("/choices/0/message/tool_calls")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_error("gateway did not return OpenAI-compatible tool_calls"))?;
    if calls.iter().any(|call| {
        call.pointer("/function/name").and_then(Value::as_str) == Some("medusa_preflight")
            && call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .is_some()
    }) {
        Ok(())
    } else {
        Err(protocol_error(
            "gateway tool_calls did not contain the requested medusa_preflight function",
        ))
    }
}

fn verify_stream(body: &str) -> MedusaResult<()> {
    let mut saw_json = false;
    let mut saw_done = false;
    for line in body.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let Some(data) = line.strip_prefix("data:") else {
            return Err(protocol_error(
                "streaming response contains a non-SSE line without a data prefix",
            ));
        };
        let data = data.trim();
        if data == "[DONE]" {
            saw_done = true;
            continue;
        }
        let value: Value = serde_json::from_str(data).map_err(|error| {
            protocol_error(format!(
                "streaming response contains malformed JSON: {error}"
            ))
        })?;
        if value.get("choices").and_then(Value::as_array).is_none() {
            return Err(protocol_error(
                "streaming event does not contain an OpenAI-compatible choices array",
            ));
        }
        saw_json = true;
    }
    if !saw_json || !saw_done {
        return Err(protocol_error(
            "streaming response did not contain both JSON events and a [DONE] terminator",
        ));
    }
    Ok(())
}

fn gateway_transport_error(error: reqwest::Error) -> MedusaError {
    preflight_error(
        ErrorCategory::Transient,
        format!("OAuth gateway transport is unavailable: {error}"),
    )
    .with_retryable(true)
}

fn protocol_error(message: impl Into<String>) -> MedusaError {
    preflight_error(
        ErrorCategory::Validation,
        format!("OAuth gateway protocol is incompatible: {}", message.into()),
    )
}

fn preflight_error(category: ErrorCategory, message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::DependencyUnavailable, category, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_coding_sessions_trigger_preflight() {
        assert!(is_coding_session(&[]));
        assert!(is_coding_session(&["run".into(), "fix tests".into()]));
        assert!(is_coding_session(&["--prompt".into(), "fix tests".into()]));
        assert!(!is_coding_session(&["doctor".into()]));
        assert!(!is_coding_session(&["config".into()]));
    }

    #[test]
    fn model_discovery_requires_requested_model() {
        verify_model(r#"{"data":[{"id":"gpt-test"}]}"#, "gpt-test").expect("model");
        assert!(verify_model(r#"{"data":[{"id":"other"}]}"#, "gpt-test").is_err());
    }

    #[test]
    fn tool_support_requires_openai_tool_calls() {
        verify_tool_call(
            r#"{"choices":[{"message":{"tool_calls":[{"function":{"name":"medusa_preflight","arguments":"{}"}}]}}]}"#,
        )
        .expect("tool call");
        assert!(verify_tool_call(r#"{"choices":[{"message":{"content":"no tool"}}]}"#).is_err());
    }

    #[test]
    fn malformed_streaming_is_rejected() {
        verify_stream("data: {\"choices\":[{\"delta\":{}}]}\n\ndata: [DONE]\n")
            .expect("valid stream");
        assert!(verify_stream("data: not-json\n\ndata: [DONE]\n").is_err());
        assert!(verify_stream("event: message\n").is_err());
    }

    #[test]
    fn expired_authentication_is_classified_separately() {
        let error = preflight_error(
            ErrorCategory::Policy,
            "gateway model discovery failed: incompatible authentication, HTTP 401 Unauthorized",
        );
        assert_eq!(error.category, ErrorCategory::Policy);
        assert!(error.message.contains("authentication"));
    }
}
