use std::{
    process::{Child, Stdio},
    thread,
    time::{Duration, Instant},
};

use medusa_config::openai_oauth;
use medusa_core::hidden_command;
use reqwest::blocking::Client;
use serde_json::Value;

const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

fn gateway_client() -> Result<Client, String> {
    Client::builder()
        .connect_timeout(Duration::from_millis(500))
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|error| format!("OAuth gateway client failed: {error}"))
}

fn parse_models(value: Value) -> Result<Vec<String>, String> {
    let mut models = value
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    models.sort();
    models.dedup();
    if models.is_empty() {
        Err("OAuth gateway returned no selectable models".to_owned())
    } else {
        Ok(models)
    }
}

pub fn discover_openai_oauth_models() -> Result<Vec<String>, String> {
    let response = gateway_client()?
        .get(format!("{}/models", openai_oauth::GATEWAY_BASE_URL))
        .send()
        .map_err(|error| format!("OAuth gateway is not ready: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "OAuth gateway model discovery returned {}",
            response.status()
        ));
    }
    let value = response
        .json::<Value>()
        .map_err(|error| format!("OAuth model discovery returned invalid JSON: {error}"))?;
    parse_models(value)
}

fn start_browser_login() -> Result<Child, String> {
    hidden_command(openai_oauth::npx_program())
        .args(openai_oauth::LOGIN_ARGS)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            format!("could not launch ChatGPT browser sign-in: {error}. Install Node.js and retry")
        })
}

pub fn ensure_openai_oauth_connected() -> Result<Vec<String>, String> {
    if let Ok(models) = discover_openai_oauth_models() {
        return Ok(models);
    }

    let mut child = start_browser_login()?;
    let deadline = Instant::now() + LOGIN_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return discover_openai_oauth_models(),
            Ok(Some(status)) => {
                return Err(format!("ChatGPT browser sign-in exited with {status}"));
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("ChatGPT browser sign-in timed out".to_owned());
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("ChatGPT browser sign-in failed: {error}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovered_models_are_sorted_and_deduplicated() {
        assert_eq!(
            parse_models(serde_json::json!({
                "data": [{"id": "gpt-b"}, {"id": "gpt-a"}, {"id": "gpt-a"}]
            }))
            .expect("models"),
            vec!["gpt-a".to_owned(), "gpt-b".to_owned()]
        );
        assert!(parse_models(serde_json::json!({"data": []})).is_err());
    }
}
