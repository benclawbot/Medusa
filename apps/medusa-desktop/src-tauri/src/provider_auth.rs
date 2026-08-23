use std::{
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    process::Stdio,
    thread,
    time::{Duration, Instant},
};

use crate::desktop_command::hidden_command;
use medusa_config::openai_oauth;

const OPENAI_OAUTH_ADDR: &str = openai_oauth::GATEWAY_ADDR;

fn npx_program() -> &'static str {
    openai_oauth::npx_program()
}

fn oauth_auth_file_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("CODEX_HOME") {
        candidates.push(PathBuf::from(home).join("auth.json"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".codex").join("auth.json"));
    }
    if let Some(home) = std::env::var_os("USERPROFILE") {
        candidates.push(PathBuf::from(home).join(".codex").join("auth.json"));
    }
    candidates
}

fn oauth_auth_file_present() -> bool {
    oauth_auth_file_candidates()
        .iter()
        .any(|path| path.is_file())
}

fn should_launch_browser_login(auth_file_present: bool) -> bool {
    !auth_file_present
}

fn browser_oauth_spec(provider: &str) -> Result<(&'static str, [&'static str; 6]), String> {
    if provider != "openai-oauth" {
        return Err(format!(
            "provider `{provider}` does not expose a Medusa browser sign-in helper"
        ));
    }
    Ok((npx_program(), openai_oauth::LOGIN_ARGS))
}

fn browser_oauth_gateway_spec(provider: &str) -> Result<(&'static str, [&'static str; 4]), String> {
    if provider != "openai-oauth" {
        return Err(format!(
            "provider `{provider}` does not expose a Medusa browser sign-in helper"
        ));
    }
    Ok((npx_program(), openai_oauth::GATEWAY_ARGS))
}

pub(crate) fn browser_oauth_credentials_present(provider: &str) -> bool {
    provider == "openai-oauth" && oauth_auth_file_present()
}

fn browser_oauth_models_present(provider: &str) -> bool {
    provider == "openai-oauth" && medusa_runtime::discover_openai_oauth_models().is_ok()
}

fn gateway_is_reachable() -> bool {
    OPENAI_OAUTH_ADDR
        .parse::<SocketAddr>()
        .ok()
        .is_some_and(|address| {
            TcpStream::connect_timeout(&address, Duration::from_millis(400)).is_ok()
        })
}

fn wait_for_oauth_gateway() -> Result<(), String> {
    let address: SocketAddr = OPENAI_OAUTH_ADDR
        .parse()
        .map_err(|error| format!("invalid OAuth gateway address: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(&address, Duration::from_millis(400)).is_ok() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
    Err(format!(
        "ChatGPT OAuth completed, but the local OAuth gateway did not become reachable at http://{OPENAI_OAUTH_ADDR}/v1"
    ))
}

fn ensure_browser_oauth_gateway(provider: &str) -> Result<(), String> {
    let (gateway_program, gateway_args) = browser_oauth_gateway_spec(provider)?;
    if gateway_is_reachable() {
        return Ok(());
    }

    let gateway_status = hidden_command(gateway_program)
        .args(gateway_args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| format!("could not start the ChatGPT OAuth gateway: {error}"))?;
    if !gateway_status.success() {
        return Err(format!(
            "ChatGPT OAuth gateway startup exited with {gateway_status}"
        ));
    }
    wait_for_oauth_gateway()
}

#[tauri::command]
pub async fn desktop_ensure_browser_oauth_gateway(provider: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || ensure_browser_oauth_gateway(&provider))
        .await
        .map_err(|error| format!("OAuth gateway startup task failed: {error}"))?
}

#[tauri::command]
pub async fn desktop_browser_oauth(provider: String) -> Result<(), String> {
    let (program, args) = browser_oauth_spec(&provider)?;
    let launch_browser_login = should_launch_browser_login(oauth_auth_file_present());
    tauri::async_runtime::spawn_blocking(move || {
        if launch_browser_login {
            let status = hidden_command(program)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|error| {
                    format!(
                        "could not launch browser sign-in with openai-oauth: {error}. Install Node.js and retry from Medusa"
                    )
                })?;
            if !status.success() {
                return Err(format!("openai-oauth browser sign-in exited with {status}"));
            }
        }
        ensure_browser_oauth_gateway(&provider)?;
        if !browser_oauth_models_present("openai-oauth") {
            return Err(
                "ChatGPT OAuth credentials are present or sign-in completed, but the OAuth gateway returned no authenticated models. Run `npx openai-oauth login` in an interactive terminal to re-authenticate"
                    .to_owned(),
            );
        }
        Ok(())
    })
    .await
    .map_err(|error| format!("browser sign-in task failed: {error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_command_matches_cli_contract() {
        let (program, args) = browser_oauth_spec("openai-oauth").expect("oauth spec");
        assert_eq!(program, npx_program());
        assert_eq!(
            args,
            [
                "--yes",
                "openai-oauth@2.0.0",
                "login",
                "--open",
                "--login-timeout-ms",
                "300000",
            ]
        );
    }

    #[test]
    fn oauth_gateway_is_started_detached() {
        let (program, args) = browser_oauth_gateway_spec("openai-oauth").expect("gateway spec");
        assert_eq!(program, npx_program());
        assert_eq!(args, ["--yes", "openai-oauth@2.0.0", "--no-open", "--detach"]);
    }

    #[test]
    fn existing_oauth_credentials_skip_noninteractive_login() {
        assert!(!should_launch_browser_login(true));
        assert!(should_launch_browser_login(false));
    }

    #[test]
    fn non_oauth_provider_is_rejected() {
        assert!(browser_oauth_spec("minimax").is_err());
        assert!(browser_oauth_gateway_spec("minimax").is_err());
    }
}
