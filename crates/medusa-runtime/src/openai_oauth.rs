use std::{
    io::{BufRead, BufReader},
    process::{Child, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use medusa_config::openai_oauth;
use medusa_core::hidden_command;
use reqwest::blocking::Client;
use serde_json::Value;

const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
const GATEWAY_START_TIMEOUT: Duration = Duration::from_secs(15);
const LOGIN_URL_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const LOGIN_URL_PREFIX: &str = "OpenAI OAuth login URL: ";

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

fn login_url_from_line(line: &str) -> Option<&str> {
    let url = line.trim().strip_prefix(LOGIN_URL_PREFIX)?;
    (url.starts_with("https://") || url.starts_with("http://")).then_some(url)
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

fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Starts the pinned OAuth helper without its broken Windows `cmd /c start` opener.
///
/// `openai-oauth@2.0.0` prints the authorization URL before waiting for the callback. Medusa
/// captures that line and opens it with a direct URL launcher so query separators such as `&`
/// are not interpreted as shell syntax.
pub fn start_openai_oauth_login() -> Result<Child, String> {
    if openai_oauth::auth_file_present() {
        return Err(
            "ChatGPT OAuth credentials already exist; re-authenticate with `npx openai-oauth@2.0.0 login --no-open` in an interactive terminal".to_owned(),
        );
    }
    let mut child = hidden_command(openai_oauth::npx_program())
        .args(openai_oauth::LOGIN_ARGS)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            format!("could not launch ChatGPT browser sign-in: {error}. Install Node.js and retry")
        })?;
    let Some(stdout) = child.stdout.take() else {
        terminate_child(&mut child);
        return Err("ChatGPT browser sign-in did not expose its output".to_owned());
    };
    let (sender, receiver) = mpsc::channel();
    if let Err(error) = thread::Builder::new()
        .name("medusa-openai-oauth-url".to_owned())
        .spawn(move || {
            let mut url_reported = false;
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) if !url_reported => {
                        if let Some(url) = login_url_from_line(&line) {
                            url_reported = true;
                            let _ = sender.send(Ok(url.to_owned()));
                        }
                    }
                    Ok(_) => {}
                    Err(error) if !url_reported => {
                        let _ = sender.send(Err(format!(
                            "could not read the ChatGPT sign-in URL: {error}"
                        )));
                        return;
                    }
                    Err(_) => return,
                }
            }
            if !url_reported {
                let _ = sender.send(Err(
                    "openai-oauth did not emit a ChatGPT sign-in URL".to_owned()
                ));
            }
        })
    {
        terminate_child(&mut child);
        return Err(format!(
            "could not monitor the ChatGPT browser sign-in URL: {error}"
        ));
    }

    match receiver.recv_timeout(LOGIN_URL_TIMEOUT) {
        Ok(Ok(url)) => {
            if let Err(error) = open_browser_url(&url) {
                terminate_child(&mut child);
                return Err(error);
            }
            Ok(child)
        }
        Ok(Err(error)) => {
            terminate_child(&mut child);
            Err(error)
        }
        Err(error) => {
            terminate_child(&mut child);
            Err(format!(
                "timed out waiting for the ChatGPT sign-in URL: {error}"
            ))
        }
    }
}

fn should_launch_browser_login(auth_file_present: bool) -> bool {
    !auth_file_present
}

fn start_gateway() -> Result<Child, String> {
    hidden_command(openai_oauth::npx_program())
        .args(openai_oauth::GATEWAY_ARGS)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("could not start the ChatGPT OAuth gateway: {error}"))
}

fn wait_for_gateway() -> Result<Vec<String>, String> {
    let mut gateway = start_gateway()?;
    let deadline = Instant::now() + GATEWAY_START_TIMEOUT;
    loop {
        if let Ok(models) = discover_openai_oauth_models() {
            return Ok(models);
        }
        if Instant::now() >= deadline {
            let _ = gateway.try_wait();
            return Err(format!(
                "OAuth gateway did not become ready at {}",
                openai_oauth::GATEWAY_ADDR
            ));
        }
        let _ = gateway.try_wait();
        thread::sleep(POLL_INTERVAL);
    }
}

pub fn ensure_openai_oauth_connected() -> Result<Vec<String>, String> {
    if let Ok(models) = discover_openai_oauth_models() {
        return Ok(models);
    }

    if !should_launch_browser_login(openai_oauth::auth_file_present()) {
        return wait_for_gateway().map_err(|error| {
            format!(
                "ChatGPT OAuth gateway is unavailable while existing credentials are present: {error}. Run `npx openai-oauth@2.0.0 login --no-open` in an interactive terminal, confirm the overwrite, and copy the printed URL into your browser to re-authenticate"
            )
        });
    }

    let mut child = start_openai_oauth_login()?;
    let deadline = Instant::now() + LOGIN_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                if let Ok(models) = discover_openai_oauth_models() {
                    return Ok(models);
                }
                return wait_for_gateway();
            }
            Ok(Some(status)) => {
                return Err(format!("ChatGPT browser sign-in exited with {status}"));
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            Ok(None) => {
                terminate_child(&mut child);
                return Err("ChatGPT browser sign-in timed out".to_owned());
            }
            Err(error) => {
                terminate_child(&mut child);
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

    #[test]
    fn existing_credentials_skip_noninteractive_browser_login() {
        assert!(!should_launch_browser_login(true));
        assert!(should_launch_browser_login(false));
    }

    #[test]
    fn login_url_parser_preserves_query_parameters() {
        let url = "https://auth.openai.com/oauth/authorize?response_type=code&client_id=test";
        let line = format!("OpenAI OAuth login URL: {url}\r\n");
        assert_eq!(login_url_from_line(&line), Some(url));
    }

    #[test]
    fn browser_launcher_keeps_oauth_url_out_of_cmd_shell_parsing() {
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
}
