use std::{
    io::{self, IsTerminal, Write},
    net::{SocketAddr, TcpStream},
    process::Command,
    time::Duration,
};

use medusa_config::{PROVIDER_PROFILE_KEYS, ProviderProfile, ProviderProfileCatalog};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::Serialize;

pub(crate) fn config_path() -> MedusaResult<std::path::PathBuf> {
    Ok(ProviderProfileCatalog::user()?.active_store()?.path().to_path_buf())
}

pub(crate) fn load_profile() -> MedusaResult<ProviderProfile> {
    ProviderProfileCatalog::user()?.active_store()?.load()
}

pub(crate) fn ensure_first_run() -> MedusaResult<()> {
    if load_profile()?.configured || !io::stdin().is_terminal() {
        return Ok(());
    }
    println!("Medusa needs a model connection before the first interactive session.\n");
    configure_interactive()
}

pub(crate) fn configure_interactive() -> MedusaResult<()> {
    let store = ProviderProfileCatalog::user()?.active_store()?;
    let mut profile = store.load()?;
    profile.connection = choose(
        "Connection type",
        &[
            (
                "omniroute",
                "OmniRoute managed/existing gateway (recommended)",
            ),
            (
                "chatgpt-oauth",
                "ChatGPT OAuth via local openai-oauth gateway",
            ),
            ("openai-api", "OpenAI API key"),
            ("openai-compatible", "Existing OpenAI-compatible endpoint"),
            ("direct", "Direct provider"),
            ("local", "Local model runtime"),
        ],
        &profile.connection,
    )?;

    profile.provider = prompt(
        "Provider or route",
        provider_default(&profile.connection, &profile.provider),
    )?;
    profile.model = prompt("Model", model_default(&profile.connection, &profile.model))?;
    profile.speed = choose(
        "Speed",
        &[
            ("fast", "Fast"),
            ("balanced", "Balanced"),
            ("quality", "Maximum quality"),
            ("custom", "Custom"),
        ],
        &profile.speed,
    )?;
    profile.reasoning = choose(
        "Thinking level",
        &[
            ("low", "Low"),
            ("medium", "Medium"),
            ("high", "High"),
            ("maximum", "Maximum"),
        ],
        &profile.reasoning,
    )?;

    if matches!(
        profile.connection.as_str(),
        "omniroute" | "chatgpt-oauth" | "openai-compatible" | "local"
    ) {
        let default_url = match profile.connection.as_str() {
            "omniroute" => "http://127.0.0.1:20128/v1",
            "chatgpt-oauth" => "http://127.0.0.1:10531/v1",
            "local" => "http://127.0.0.1:11434/v1",
            _ => profile
                .base_url
                .as_deref()
                .unwrap_or("http://127.0.0.1:8000/v1"),
        };
        profile.base_url = Some(prompt("Base URL", default_url)?);
    } else {
        profile.base_url = None;
    }

    if profile.connection == "chatgpt-oauth" {
        profile.provider = "openai-oauth".into();
        profile.auth = "none".into();
        profile.base_url = Some("http://127.0.0.1:10531/v1".into());
    } else if profile.connection == "openai-api" {
        profile.provider = "openai".into();
        profile.auth = "api-key".into();
        profile.base_url = Some("https://api.openai.com/v1".into());
    } else {
        profile.auth = choose(
            "Authentication",
            &[
                ("oauth", "OAuth / browser sign-in"),
                ("api-key", "API key"),
                ("existing", "Existing environment or gateway credentials"),
                ("none", "No authentication"),
            ],
            &profile.auth,
        )?;
    }
    profile.configured = true;
    store.save(&profile)?;
    ensure_runtime_for_profile(&profile)?;

    println!("\nConfiguration saved to {}", store.path().display());
    print_auth_guidance(&profile);
    Ok(())
}

pub(crate) fn show(json: bool) -> MedusaResult<()> {
    let profile = load_profile()?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&profile)
                .map_err(|error| config_error(error.to_string()))?
        );
    } else {
        println!(
            "{}",
            toml::to_string_pretty(&profile).map_err(|error| config_error(error.to_string()))?
        );
    }
    Ok(())
}

pub(crate) fn get(key: &str, json: bool) -> MedusaResult<()> {
    let profile = load_profile()?;
    let value = profile.value(key).ok_or_else(|| {
        config_error(format!(
            "unknown configuration key `{key}`; available keys: {}",
            PROVIDER_PROFILE_KEYS.join(", ")
        ))
    })?;
    if json {
        println!(
            "{}",
            serde_json::to_string(&value).map_err(|error| config_error(error.to_string()))?
        );
    } else {
        println!("{value}");
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct ValidationOutput {
    valid: bool,
    configured: bool,
    path: String,
    provider: String,
    model: String,
    auth: String,
    protocol: String,
    openai_oauth: bool,
}

pub(crate) fn validate(json: bool) -> MedusaResult<()> {
    let store = ProviderProfileCatalog::user()?.active_store()?;
    let profile = store.load()?;
    profile.validate()?;
    let output = ValidationOutput {
        valid: true,
        configured: profile.configured,
        path: store.path().display().to_string(),
        provider: profile.provider.clone(),
        model: profile.model.clone(),
        auth: profile.auth.clone(),
        protocol: profile.protocol().to_owned(),
        openai_oauth: profile.uses_openai_oauth(),
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&output)
                .map_err(|error| config_error(error.to_string()))?
        );
    } else {
        println!("Configuration is valid.");
        println!("Path: {}", output.path);
        println!("Configured: {}", output.configured);
        println!(
            "Route: {} / {} ({}, auth: {})",
            output.provider, output.model, output.protocol, output.auth
        );
        println!("OpenAI OAuth route: {}", output.openai_oauth);
    }
    Ok(())
}

pub(crate) fn reset() -> MedusaResult<()> {
    let store = ProviderProfileCatalog::user()?.active_store()?;
    let removed = store.reset()?;
    if removed {
        println!("Medusa provider configuration reset.");
    } else {
        println!("Medusa provider configuration was already empty.");
    }
    Ok(())
}

fn choose(title: &str, choices: &[(&str, &str)], current: &str) -> MedusaResult<String> {
    println!("{title}:");
    for (index, (value, label)) in choices.iter().enumerate() {
        let marker = if *value == current { "*" } else { " " };
        println!("  {}. [{marker}] {label}", index + 1);
    }
    let default_selection = choices
        .iter()
        .position(|(value, _)| *value == current)
        .map(|index| (index + 1).to_string())
        .unwrap_or_else(|| "1".to_owned());
    let raw = prompt("Selection", &default_selection)?;
    let index = raw
        .parse::<usize>()
        .map_err(|_| config_error(format!("invalid selection: {raw}")))?;
    choices
        .get(index.saturating_sub(1))
        .map(|(value, _)| (*value).to_owned())
        .ok_or_else(|| config_error(format!("selection out of range: {index}")))
}

fn prompt(label: &str, default: &str) -> MedusaResult<String> {
    print!("{label} [{default}]: ");
    io::stdout()
        .flush()
        .map_err(|error| config_error(error.to_string()))?;
    let mut value = String::new();
    io::stdin()
        .read_line(&mut value)
        .map_err(|error| config_error(error.to_string()))?;
    let value = value.trim();
    Ok(if value.is_empty() {
        default.to_owned()
    } else {
        value.to_owned()
    })
}

fn provider_default<'a>(connection: &str, current: &'a str) -> &'a str {
    if current.is_empty() {
        match connection {
            "omniroute" => "auto/coding",
            "local" => "local",
            _ => "minimax",
        }
    } else {
        current
    }
}

fn model_default<'a>(connection: &str, current: &'a str) -> &'a str {
    if current.is_empty() {
        if connection == "omniroute" {
            "auto/coding"
        } else {
            "MiniMax-M3"
        }
    } else {
        current
    }
}

pub(crate) fn ensure_selected_runtime() -> MedusaResult<()> {
    let profile = load_profile()?;
    if profile.configured {
        ensure_runtime_for_profile(&profile)?;
    }
    Ok(())
}

fn ensure_runtime_for_profile(profile: &ProviderProfile) -> MedusaResult<()> {
    if profile.uses_openai_oauth() {
        ensure_chatgpt_oauth_gateway()?;
    }
    Ok(())
}

fn ensure_chatgpt_oauth_gateway() -> MedusaResult<()> {
    let address: SocketAddr = "127.0.0.1:10531"
        .parse()
        .map_err(|error| config_error(format!("invalid OAuth gateway address: {error}")))?;
    if TcpStream::connect_timeout(&address, Duration::from_millis(300)).is_ok() {
        return Ok(());
    }

    println!("Starting the local ChatGPT OAuth gateway...");
    let status = Command::new("npx")
        .args(["--yes", "openai-oauth@latest", "--detach"])
        .status()
        .map_err(|error| {
            config_error(format!(
                "could not start openai-oauth with npx: {error}. Install Node.js or start the gateway manually"
            ))
        })?;
    if !status.success() {
        return Err(config_error(format!(
            "openai-oauth exited with status {status}; run `npx openai-oauth@latest login` and retry"
        )));
    }
    if TcpStream::connect_timeout(&address, Duration::from_secs(2)).is_err() {
        return Err(config_error(
            "openai-oauth started but its loopback endpoint is not reachable at 127.0.0.1:10531",
        ));
    }
    Ok(())
}

fn print_auth_guidance(profile: &ProviderProfile) {
    if profile.uses_openai_oauth() {
        println!(
            "ChatGPT OAuth is managed by the loopback gateway at http://127.0.0.1:10531/v1; Medusa never reads the OAuth credential file."
        );
        return;
    }
    match profile.auth.as_str() {
        "oauth" if profile.connection == "omniroute" => println!(
            "Complete provider OAuth in the OmniRoute dashboard; Medusa will use the local gateway credential."
        ),
        "oauth" => println!(
            "OAuth support is provider-specific and will open the provider login flow when the adapter supports it."
        ),
        "api-key" => println!(
            "API keys are not written to provider.toml. Existing provider environment variables remain supported."
        ),
        "existing" => println!(
            "Medusa will use credentials already available to the selected gateway or provider."
        ),
        _ => {}
    }
}

fn config_error(message: impl Into<String>) -> MedusaError {
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
    fn defaults_are_safe_and_backward_compatible() {
        let profile = ProviderProfile::default();
        assert_eq!(profile.connection, "direct");
        assert_eq!(profile.provider, "minimax");
        assert_eq!(profile.model, "MiniMax-M3");
        assert!(!profile.configured);
    }

    #[test]
    fn profile_round_trips_without_secrets() {
        let profile = ProviderProfile {
            configured: true,
            connection: "omniroute".into(),
            base_url: Some("http://127.0.0.1:20128/v1".into()),
            ..ProviderProfile::default()
        };
        profile.validate().expect("profile");
        let encoded = toml::to_string(&profile).expect("serialize");
        assert!(!encoded.to_ascii_lowercase().contains("api_key"));
        let decoded: ProviderProfile = toml::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, profile);
    }

    #[test]
    fn keyed_values_are_stable_and_redacted() {
        let profile = ProviderProfile::default();
        assert_eq!(
            profile.value("provider"),
            Some(medusa_config::ProviderProfileValue::String(
                "minimax".into()
            ))
        );
        assert!(profile.value("token").is_none());
    }
}
