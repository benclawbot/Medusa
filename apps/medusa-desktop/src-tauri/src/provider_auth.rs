use std::process::Stdio;

use crate::desktop_command::hidden_command;

fn browser_oauth_spec(provider: &str) -> Result<(&'static str, [&'static str; 6]), String> {
    if provider != "openai-oauth" {
        return Err(format!(
            "provider `{provider}` does not expose a Medusa browser sign-in helper"
        ));
    }
    Ok((
        "npx",
        [
            "--yes",
            "openai-oauth@latest",
            "login",
            "--open",
            "--login-timeout-ms",
            "300000",
        ],
    ))
}

#[tauri::command]
pub async fn desktop_browser_oauth(provider: String) -> Result<(), String> {
    let (program, args) = browser_oauth_spec(&provider)?;
    tauri::async_runtime::spawn_blocking(move || {
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
        if status.success() {
            Ok(())
        } else {
            Err(format!("openai-oauth browser sign-in exited with {status}"))
        }
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
        assert_eq!(program, "npx");
        assert_eq!(
            args,
            [
                "--yes",
                "openai-oauth@latest",
                "login",
                "--open",
                "--login-timeout-ms",
                "300000",
            ]
        );
    }

    #[test]
    fn non_oauth_provider_is_rejected() {
        assert!(browser_oauth_spec("minimax").is_err());
    }
}
