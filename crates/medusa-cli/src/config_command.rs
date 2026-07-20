use std::{
    env, fs,
    io::{self, IsTerminal, Write},
    path::PathBuf,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ProviderProfile {
    pub connection: String,
    pub provider: String,
    pub model: String,
    pub speed: String,
    pub reasoning: String,
    pub auth: String,
    pub base_url: Option<String>,
    pub configured: bool,
}

impl Default for ProviderProfile {
    fn default() -> Self {
        Self {
            connection: "direct".into(),
            provider: "minimax".into(),
            model: "MiniMax-M3".into(),
            speed: "balanced".into(),
            reasoning: "medium".into(),
            auth: "api-key".into(),
            base_url: None,
            configured: false,
        }
    }
}

pub(crate) fn config_path() -> MedusaResult<PathBuf> {
    let base = if cfg!(windows) {
        env::var_os("APPDATA").map(PathBuf::from)
    } else if let Some(path) = env::var_os("XDG_CONFIG_HOME") {
        Some(PathBuf::from(path))
    } else {
        env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
    }
    .ok_or_else(|| {
        MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Environment,
            "could not resolve the user configuration directory",
        )
    })?;
    Ok(base.join("medusa").join("provider.toml"))
}

pub(crate) fn load_profile() -> MedusaResult<ProviderProfile> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(ProviderProfile::default());
    }
    let text = fs::read_to_string(&path)
        .map_err(|error| config_error(format!("read {}: {error}", path.display())))?;
    toml::from_str(&text)
        .map_err(|error| config_error(format!("parse {}: {error}", path.display())))
}

pub(crate) fn ensure_first_run() -> MedusaResult<()> {
    if load_profile()?.configured || !io::stdin().is_terminal() {
        return Ok(());
    }
    println!("Medusa needs a model connection before the first interactive session.\n");
    configure_interactive()
}

pub(crate) fn configure_interactive() -> MedusaResult<()> {
    let mut profile = load_profile()?;
    profile.connection = choose(
        "Connection type",
        &[
            (
                "omniroute",
                "OmniRoute managed/existing gateway (recommended)",
            ),
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
        "omniroute" | "openai-compatible" | "local"
    ) {
        let default_url = match profile.connection.as_str() {
            "omniroute" => "http://127.0.0.1:20128/v1",
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
    profile.configured = true;
    save_profile(&profile)?;

    println!("\nConfiguration saved to {}", config_path()?.display());
    print_auth_guidance(&profile);
    Ok(())
}

pub(crate) fn show() -> MedusaResult<()> {
    let profile = load_profile()?;
    println!(
        "{}",
        toml::to_string_pretty(&profile).map_err(|error| config_error(error.to_string()))?
    );
    Ok(())
}

pub(crate) fn reset() -> MedusaResult<()> {
    let path = config_path()?;
    if path.exists() {
        fs::remove_file(&path)
            .map_err(|error| config_error(format!("remove {}: {error}", path.display())))?;
    }
    println!("Medusa provider configuration reset.");
    Ok(())
}

fn save_profile(profile: &ProviderProfile) -> MedusaResult<()> {
    let path = config_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| config_error("configuration path has no parent"))?;
    fs::create_dir_all(parent)
        .map_err(|error| config_error(format!("create {}: {error}", parent.display())))?;
    let text = toml::to_string_pretty(profile).map_err(|error| config_error(error.to_string()))?;
    let temporary = path.with_extension("toml.tmp");
    fs::write(&temporary, text)
        .map_err(|error| config_error(format!("write {}: {error}", temporary.display())))?;
    fs::rename(&temporary, &path)
        .map_err(|error| config_error(format!("replace {}: {error}", path.display())))?;
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

fn print_auth_guidance(profile: &ProviderProfile) {
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
        let encoded = toml::to_string(&profile).expect("serialize");
        assert!(!encoded.to_ascii_lowercase().contains("api_key"));
        let decoded: ProviderProfile = toml::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded.connection, "omniroute");
    }
}
