use std::{
    collections::BTreeMap,
    env,
    io::{self, IsTerminal, Write},
    path::Path,
    process::Command,
};

use medusa_config::{
    Config, PROVIDER_PROFILE_KEYS, ConfigurationApplyTiming, ConfigurationChangeOrigin,
    ConfigurationChanged, ProviderProfile, ProviderProfileCatalog, ProviderProfileStore,
    credential_environment, diagnose_config_catalog,
};
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
    let catalog = ProviderProfileCatalog::user()?;
    let snapshot = catalog.snapshot()?;
    let store = catalog.active_store()?;
    let mut profile = snapshot.profile;

    println!("Medusa model setup");
    println!("Use Up/Down to move, Enter to select, and Esc to cancel menu steps.");
    println!("Text fields keep their current value when you press Enter.\n");

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
    Config::load_layers_with_provider_profile(
        &profile,
        None,
        None,
        &BTreeMap::new(),
        &BTreeMap::new(),
    )?;

    print_profile_summary(&profile);
    let decision = choose(
        "Save configuration",
        &[
            ("save", "Save and continue"),
            ("cancel", "Cancel without saving"),
        ],
        "save",
    )?;
    if decision != "save" {
        println!("Configuration unchanged.");
        return Ok(());
    }

    let change = catalog.save_active_profile(
        &profile,
        snapshot.revision,
        ConfigurationChangeOrigin::Cli,
        PROVIDER_PROFILE_KEYS.iter().map(|key| (*key).to_owned()),
        ConfigurationApplyTiming::NextSession,
    )?;
    ensure_runtime_for_profile(&profile)?;

    println!(
        "\nConfiguration saved to {} at revision {}",
        store.path().display(),
        change.revision
    );
    print_auth_guidance(&profile);
    Ok(())
}

fn print_profile_summary(profile: &ProviderProfile) {
    println!("\nConfiguration summary");
    println!("  Connection: {}", profile.connection);
    println!("  Provider:   {}", profile.provider);
    println!("  Model:      {}", profile.model);
    println!("  Protocol:   {}", profile.protocol());
    println!("  Speed:      {}", profile.speed);
    println!("  Thinking:   {}", profile.reasoning);
    println!("  Auth:       {}", profile.auth);
    if let Some(base_url) = profile.base_url.as_deref() {
        println!("  Base URL:   {base_url}");
    }
    if let Some(variable) = credential_environment(&profile.provider)
        && profile.auth == "api-key"
    {
        println!("  Credential: {variable} (environment only)");
    }
    println!();
}

#[derive(Debug, Serialize)]
struct ConfigShowOutput {
    revision: u64,
    active_profile: String,
    profile: ProviderProfile,
}

pub(crate) fn show(json: bool) -> MedusaResult<()> {
    let snapshot = ProviderProfileCatalog::user()?.snapshot()?;
    let output = ConfigShowOutput {
        revision: snapshot.revision,
        active_profile: snapshot.active_profile,
        profile: snapshot.profile,
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&output)
                .map_err(|error| config_error(error.to_string()))?
        );
    } else {
        println!(
            "{}",
            toml::to_string_pretty(&output).map_err(|error| config_error(error.to_string()))?
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
    revision: u64,
    active_profile: String,
    configured: bool,
    path: String,
    provider: String,
    model: String,
    auth: String,
    protocol: String,
    openai_oauth: bool,
}

pub(crate) fn validate(json: bool) -> MedusaResult<()> {
    let catalog = ProviderProfileCatalog::user()?;
    let snapshot = catalog.snapshot()?;
    let store = catalog.active_store()?;
    let profile = snapshot.profile;
    profile.validate()?;
    let output = ValidationOutput {
        valid: true,
        revision: snapshot.revision,
        active_profile: snapshot.active_profile,
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
        println!("Revision: {}", output.revision);
        println!("Active profile: {}", output.active_profile);
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

pub(crate) fn edit() -> MedusaResult<()> {
    let catalog = ProviderProfileCatalog::user()?;
    let snapshot = catalog.snapshot()?;
    let store = catalog.active_store()?;
    let edit_path = store.path().with_extension(format!(
        "toml.edit-{}",
        std::process::id()
    ));
    let edit_store = ProviderProfileStore::at(edit_path);
    edit_store.save(&snapshot.profile)?;

    let status = launch_editor(edit_store.path())?;
    if !status.success() {
        let _ = edit_store.reset();
        return Err(config_error(format!(
            "configuration editor exited with status {status}; shared configuration was not changed"
        )));
    }

    let result = commit_edited_profile(&catalog, &edit_store, snapshot.revision);
    let _ = edit_store.reset();
    let (edited, change) = result?;
    println!(
        "Configuration saved to {} at revision {}",
        store.path().display(),
        change.revision
    );
    print_auth_guidance(&edited);
    Ok(())
}

fn launch_editor(path: &Path) -> MedusaResult<std::process::ExitStatus> {
    let specification = env::var("VISUAL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| env::var("EDITOR").ok().filter(|value| !value.trim().is_empty()))
        .unwrap_or_else(|| {
            if cfg!(windows) {
                "notepad".to_owned()
            } else {
                "vi".to_owned()
            }
        });
    let mut parts = specification.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| config_error("configured editor command is empty"))?;
    Command::new(program)
        .args(parts)
        .arg(path)
        .status()
        .map_err(|error| {
            config_error(format!(
                "could not launch configuration editor `{program}`: {error}"
            ))
        })
}

fn commit_edited_profile(
    catalog: &ProviderProfileCatalog,
    edit_store: &ProviderProfileStore,
    expected_revision: u64,
) -> MedusaResult<(ProviderProfile, ConfigurationChanged)> {
    let candidate = edit_store.load().map_err(|_| {
        config_error("edited configuration was invalid; shared configuration was not changed")
    })?;
    Config::load_layers_with_provider_profile(
        &candidate,
        None,
        None,
        &BTreeMap::new(),
        &BTreeMap::new(),
    )
    .map_err(|_| {
        config_error("edited configuration was invalid; shared configuration was not changed")
    })?;
    let change = catalog.save_active_profile(
        &candidate,
        expected_revision,
        ConfigurationChangeOrigin::Cli,
        PROVIDER_PROFILE_KEYS.iter().map(|key| (*key).to_owned()),
        ConfigurationApplyTiming::NextSession,
    )?;
    Ok((candidate, change))
}

pub(crate) fn doctor(json: bool) -> MedusaResult<()> {
    let catalog = ProviderProfileCatalog::user()?;
    let report = diagnose_config_catalog(&catalog)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| config_error(error.to_string()))?
        );
        return Ok(());
    }

    println!("Configuration doctor");
    println!("Revision: {}", report.revision);
    println!("Active profile: {}", report.active_profile);
    println!("Path: {}", report.path);
    for check in &report.checks {
        println!("[{}] {}: {}", check.status.label(), check.name, check.detail);
        if let Some(remediation) = &check.remediation {
            println!("      remediation: {remediation}");
        }
    }
    println!("Status: {}", report.summary_label());
    Ok(())
}

pub(crate) fn reset() -> MedusaResult<()> {
    let catalog = ProviderProfileCatalog::user()?;
    let revision = catalog.revision()?;
    let (removed, change) =
        catalog.reset_active_profile(revision, ConfigurationChangeOrigin::Cli)?;
    if removed {
        println!(
            "Medusa provider configuration reset at revision {}.",
            change.revision
        );
    } else {
        println!(
            "Medusa provider configuration was already empty; revision {} records the reset request.",
            change.revision
        );
    }
    Ok(())
}

fn choose(title: &str, choices: &[(&str, &str)], current: &str) -> MedusaResult<String> {
    if choices.is_empty() {
        return Err(config_error(format!("{title} has no available choices")));
    }
    let initial = current_choice_index(choices, current);
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        let labels = choices
            .iter()
            .map(|(_, label)| *label)
            .collect::<Vec<_>>();
        let selection = medusa_tui::input::select_menu(title, &labels, initial)
            .map_err(|error| config_error(format!("interactive menu failed: {error}")))?;
        return selection
            .and_then(|index| choices.get(index))
            .map(|(value, _)| (*value).to_owned())
            .ok_or_else(|| config_error("configuration cancelled"));
    }
    choose_line(title, choices, initial)
}

fn current_choice_index(choices: &[(&str, &str)], current: &str) -> usize {
    choices
        .iter()
        .position(|(value, _)| *value == current)
        .unwrap_or(0)
}

fn choose_line(
    title: &str,
    choices: &[(&str, &str)],
    default_index: usize,
) -> MedusaResult<String> {
    println!("{title}:");
    for (index, (_, label)) in choices.iter().enumerate() {
        let marker = if index == default_index { "*" } else { " " };
        println!("  {}. [{marker}] {label}", index + 1);
    }
    let default_selection = (default_index + 1).to_string();
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
        ensure_profile_ready(&profile)?;
    }
    Ok(())
}

fn ensure_profile_ready(profile: &ProviderProfile) -> MedusaResult<()> {
    if profile.auth == "api-key" {
        let variable = credential_environment(&profile.provider).ok_or_else(|| {
            config_error(format!(
                "provider `{}` has no registered API-key environment variable; run `medusa config doctor`",
                profile.provider
            ))
        })?;
        if env::var_os(variable).is_none() {
            return Err(config_error(format!(
                "{variable} is not present in the Medusa process environment. Set it before opening the TUI (PowerShell: `$env:{variable} = \"...\"`) and run `medusa config doctor`."
            )));
        }
    }

    if let Some(base_url) = profile.base_url.as_deref()
        && let Some(address) = loopback_socket(base_url)
        && TcpStream::connect_timeout(&address, Duration::from_millis(500)).is_err()
    {
        return Err(config_error(format!(
            "the configured local provider endpoint {base_url} is not reachable; start the gateway/runtime and run `medusa config doctor`"
        )));
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

    #[test]
    fn invalid_manual_edit_rolls_back_without_echoing_secret_text() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = ProviderProfileStore::at(directory.path().join("provider.toml"));
        let previous = ProviderProfile {
            configured: true,
            ..ProviderProfile::default()
        };
        store.save(&previous).expect("save previous");
        fs::write(
            store.path(),
            "connection = 'direct'\nprovider = 'minimax'\nmodel = 'MiniMax-M3'\nspeed = 'balanced'\nreasoning = 'medium'\nauth = 'api-key'\nconfigured = true\ntoken = 'super-secret-value'\n",
        )
        .expect("write invalid edit");

        let catalog = ProviderProfileCatalog::at(directory.path());
        let edit_store = ProviderProfileStore::at(directory.path().join("provider.edit.toml"));
        fs::rename(store.path(), edit_store.path()).expect("move invalid edit");
        store.save(&previous).expect("restore shared profile");
        let error = commit_edited_profile(&catalog, &edit_store, 0).expect_err("invalid edit");
        assert!(!error.to_string().contains("super-secret-value"));
        assert_eq!(store.load().expect("unchanged"), previous);
    }

    #[test]
    fn doctor_report_redacts_malformed_profile_contents() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = ProviderProfileStore::at(directory.path().join("provider.toml"));
        fs::write(store.path(), "token = 'super-secret-value'\n").expect("malformed profile");
        let report = diagnose_store(&store, 0, "default".to_owned());
        let encoded = serde_json::to_string(&report).expect("doctor json");
        assert!(!report.healthy);
        assert!(!encoded.contains("super-secret-value"));
    }

    #[test]
    fn current_choice_is_highlighted_and_unknown_values_use_first_option() {
        let choices = [("one", "One"), ("two", "Two"), ("three", "Three")];
        assert_eq!(current_choice_index(&choices, "two"), 1);
        assert_eq!(current_choice_index(&choices, "missing"), 0);
    }
}
