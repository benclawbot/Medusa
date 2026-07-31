use std::{
    collections::BTreeMap,
    env, fs,
    fs::OpenOptions,
    io::{self, IsTerminal, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    path::Path,
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use medusa_config::{
    Config, PROVIDER_PROFILE_KEYS, ConfigurationApplyTiming, ConfigurationChangeOrigin,
    ConfigurationChanged, ProviderProfile, ProviderProfileCatalog, ProviderProfileStore,
    credential_environment,
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

#[derive(Debug, Serialize)]
struct ConfigDoctorOutput {
    healthy: bool,
    revision: u64,
    active_profile: String,
    configured: bool,
    path: String,
    checks: Vec<ConfigDoctorCheck>,
}

#[derive(Debug, Serialize)]
struct ConfigDoctorCheck {
    name: &'static str,
    ok: bool,
    detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    remediation: Option<String>,
}

pub(crate) fn doctor(json: bool) -> MedusaResult<()> {
    let catalog = ProviderProfileCatalog::user()?;
    let revision = catalog.revision()?;
    let active_profile = catalog.active_name()?;
    let store = catalog.active_store()?;
    let report = diagnose_store(&store, revision, active_profile);
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
        let marker = if check.ok { "ok" } else { "fix" };
        println!("[{marker}] {}: {}", check.name, check.detail);
        if let Some(remediation) = &check.remediation {
            println!("      remediation: {remediation}");
        }
    }
    println!(
        "Status: {}",
        if report.healthy { "healthy" } else { "attention required" }
    );
    Ok(())
}

fn diagnose_store(
    store: &ProviderProfileStore,
    revision: u64,
    active_profile: String,
) -> ConfigDoctorOutput {
    let path = store.path().display().to_string();
    let mut checks = Vec::new();
    let exists = store.path().is_file();
    add_check(
        &mut checks,
        "profile_file",
        exists,
        if exists {
            "active profile exists"
        } else {
            "active profile has not been initialized"
        },
        (!exists).then_some("run `medusa config init`"),
    );
    let profile_store_writable = profile_store_writable(store.path());
    add_check(
        &mut checks,
        "profile_store",
        profile_store_writable,
        if profile_store_writable {
            "configuration directory is writable"
        } else {
            "configuration directory is not writable or does not exist"
        },
        Some("fix the configuration-directory ownership or permissions"),
    );

    let profile = match store.load() {
        Ok(profile) => {
            add_check(
                &mut checks,
                "schema",
                true,
                "profile schema and typed invariants are valid",
                None::<String>,
            );
            profile
        }
        Err(_) => {
            add_check(
                &mut checks,
                "schema",
                false,
                "profile cannot be parsed or validated",
                Some("run `medusa config edit` or `medusa config reset`"),
            );
            return finish_doctor(path, revision, active_profile, false, checks);
        }
    };

    add_check(
        &mut checks,
        "configured",
        profile.configured,
        if profile.configured {
            "provider profile is configured"
        } else {
            "provider profile is not configured"
        },
        (!profile.configured).then_some("run `medusa config init`"),
    );
    let effective = Config::load_layers_with_provider_profile(
        &profile,
        None,
        None,
        &BTreeMap::new(),
        &BTreeMap::new(),
    )
    .is_ok();
    add_check(
        &mut checks,
        "effective_configuration",
        effective,
        if effective {
            "resolved runtime configuration is valid"
        } else {
            "resolved runtime configuration is invalid"
        },
        (!effective).then_some("run `medusa config validate` and correct the reported field"),
    );

    diagnose_endpoint(&profile, &mut checks);
    diagnose_credential(&profile, &mut checks);
    finish_doctor(path, revision, active_profile, profile.configured, checks)
}

fn diagnose_endpoint(profile: &ProviderProfile, checks: &mut Vec<ConfigDoctorCheck>) {
    let Some(base_url) = profile.base_url.as_deref() else {
        add_check(
            checks,
            "endpoint",
            true,
            "selected route uses its provider default endpoint",
            None::<String>,
        );
        return;
    };
    let parsed = reqwest::Url::parse(base_url);
    add_check(
        checks,
        "endpoint",
        parsed.is_ok(),
        if parsed.is_ok() {
            "endpoint URL is syntactically valid"
        } else {
            "endpoint URL is invalid"
        },
        parsed
            .is_err()
            .then_some("set a valid http:// or https:// provider endpoint"),
    );
    let Some(address) = loopback_socket(base_url) else {
        return;
    };
    let reachable = TcpStream::connect_timeout(&address, Duration::from_millis(300)).is_ok();
    add_check(
        checks,
        "local_gateway",
        reachable,
        if reachable {
            "selected loopback gateway is reachable"
        } else {
            "selected loopback gateway is not reachable"
        },
        (!reachable).then_some(match profile.connection.as_str() {
            "chatgpt-oauth" => "run `npx openai-oauth@latest login` and retry",
            "omniroute" => "start OmniRoute on the configured loopback endpoint",
            "local" => "start the configured local model runtime",
            _ => "start the configured loopback gateway",
        }),
    );
}

fn diagnose_credential(profile: &ProviderProfile, checks: &mut Vec<ConfigDoctorCheck>) {
    if profile.auth != "api-key" {
        add_check(
            checks,
            "credential",
            true,
            match profile.auth.as_str() {
                "none" => "selected route does not require a Medusa-managed credential",
                "existing" => "credential ownership is delegated to the selected gateway",
                "oauth" => "credential ownership is delegated to the provider OAuth route",
                _ => "credential mode is valid",
            },
            None::<String>,
        );
        return;
    }
    let Some(variable) = credential_environment(&profile.provider) else {
        add_check(
            checks,
            "credential",
            false,
            "selected provider has no registered API-key environment source",
            Some("select a supported provider route or use an existing gateway credential"),
        );
        return;
    };
    let configured = env::var_os(variable).is_some();
    add_check(
        checks,
        "credential",
        configured,
        if configured {
            format!("{variable} is present; its value was not read or displayed")
        } else {
            format!("{variable} is not present")
        },
        (!configured).then(|| format!("set {variable} in the Medusa process environment")),
    );
}

fn loopback_socket(base_url: &str) -> Option<SocketAddr> {
    let url = reqwest::Url::parse(base_url).ok()?;
    let host = url.host_str()?;
    let address = if host.eq_ignore_ascii_case("localhost") {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        host.parse::<IpAddr>().ok()?
    };
    if !address.is_loopback() {
        return None;
    }
    Some(SocketAddr::new(address, url.port_or_known_default()?))
}

fn profile_store_writable(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    if !parent.is_dir() {
        return false;
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let probe = parent.join(format!(
        ".medusa-config-doctor-{}-{stamp}.tmp",
        std::process::id()
    ));
    let result = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .and_then(|mut file| {
            file.write_all(b"medusa-config-doctor")?;
            file.sync_all()
        })
        .is_ok();
    let _ = fs::remove_file(probe);
    result
}

fn add_check(
    checks: &mut Vec<ConfigDoctorCheck>,
    name: &'static str,
    ok: bool,
    detail: impl Into<String>,
    remediation: Option<impl Into<String>>,
) {
    checks.push(ConfigDoctorCheck {
        name,
        ok,
        detail: detail.into(),
        remediation: remediation.map(Into::into),
    });
}

fn finish_doctor(
    path: String,
    revision: u64,
    active_profile: String,
    configured: bool,
    checks: Vec<ConfigDoctorCheck>,
) -> ConfigDoctorOutput {
    ConfigDoctorOutput {
        healthy: checks.iter().all(|check| check.ok),
        revision,
        active_profile,
        configured,
        path,
        checks,
    }
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
            "connection = 'direct'
provider = 'minimax'
model = 'MiniMax-M3'
speed = 'balanced'
reasoning = 'medium'
auth = 'api-key'
configured = true
token = 'super-secret-value'
",
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
        fs::write(store.path(), "token = 'super-secret-value'
").expect("malformed profile");
        let report = diagnose_store(&store, 0, "default".to_owned());
        let encoded = serde_json::to_string(&report).expect("doctor json");
        assert!(!report.healthy);
        assert!(!encoded.contains("super-secret-value"));
    }
}
