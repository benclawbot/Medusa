use std::sync::atomic::{AtomicBool, Ordering};

use medusa_runtime::{discover_openai_oauth_models, ensure_openai_oauth_connected};

static OAUTH_AUTHENTICATED: AtomicBool = AtomicBool::new(false);

fn validate_browser_oauth_provider(provider: &str) -> Result<(), String> {
    if provider != "openai-oauth" {
        return Err(format!(
            "provider `{provider}` does not expose a Medusa browser sign-in helper"
        ));
    }
    Ok(())
}

/// Reports the last successful Codex app-server authentication in this desktop process.
pub(crate) fn browser_oauth_authenticated(provider: &str) -> bool {
    provider == "openai-oauth" && OAUTH_AUTHENTICATED.load(Ordering::Acquire)
}

fn mark_browser_oauth_authenticated() {
    OAUTH_AUTHENTICATED.store(true, Ordering::Release);
}

/// Warm the direct app-server route when already authenticated. A missing account is expected
/// during provider selection; the explicit sign-in command owns browser login.
fn ensure_browser_oauth(provider: &str) -> Result<(), String> {
    validate_browser_oauth_provider(provider)?;
    match discover_openai_oauth_models() {
        Ok(_) => {
            mark_browser_oauth_authenticated();
            Ok(())
        }
        Err(error) if error.contains("not signed in") => Ok(()),
        Err(error) => Err(format!("Codex app-server OAuth preflight failed: {error}")),
    }
}

#[tauri::command]
pub async fn desktop_ensure_browser_oauth(provider: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || ensure_browser_oauth(&provider))
        .await
        .map_err(|error| format!("OAuth app-server preflight task failed: {error}"))?
}

#[tauri::command]
pub async fn desktop_browser_oauth(provider: String) -> Result<(), String> {
    validate_browser_oauth_provider(&provider)?;
    tauri::async_runtime::spawn_blocking(|| {
        let models = ensure_openai_oauth_connected()
            .map_err(|error| format!("ChatGPT browser sign-in through Codex failed: {error}"))?;
        if models.is_empty() {
            return Err("Codex app-server returned no authenticated ChatGPT models".to_owned());
        }
        mark_browser_oauth_authenticated();
        Ok(())
    })
    .await
    .map_err(|error| format!("browser sign-in task failed: {error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_provider_is_validated_without_gateway_state() {
        validate_browser_oauth_provider("openai-oauth").expect("oauth provider");
        assert!(validate_browser_oauth_provider("minimax").is_err());
    }

    #[test]
    fn non_oauth_provider_is_not_reported_as_authenticated() {
        assert!(!browser_oauth_authenticated("minimax"));
    }
}
