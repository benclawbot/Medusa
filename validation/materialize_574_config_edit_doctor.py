from pathlib import Path


def replace_once(source: str, old: str, new: str, label: str) -> str:
    if new in source:
        return source
    count = source.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one target, found {count}")
    return source.replace(old, new, 1)


main_path = Path("crates/medusa-cli/src/main.rs")
main = main_path.read_text()
main = replace_once(
    main,
    '''use config_command::{
    configure_interactive, ensure_first_run, ensure_selected_runtime, get as get_config,
    reset as reset_config, show as show_config, validate as validate_config,
};
''',
    '''use config_command::{
    configure_interactive, doctor as doctor_config, edit as edit_config, ensure_first_run,
    ensure_selected_runtime, get as get_config, reset as reset_config, show as show_config,
    validate as validate_config,
};
''',
    "config command imports",
)
main = replace_once(
    main,
    '''    /// Print the non-secret provider profile.
    Show {
        /// Emit stable machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print one non-secret provider-profile key.
''',
    '''    /// Print the non-secret provider profile.
    Show {
        /// Emit stable machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Edit the active provider profile and commit it only when the complete result is valid.
    Edit,
    /// Print one non-secret provider-profile key.
''',
    "config edit command",
)
main = replace_once(
    main,
    '''    /// Validate the shared provider profile without billable provider work.
    Validate {
        /// Emit stable machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Remove the active provider profile so setup runs again.
''',
    '''    /// Validate the shared provider profile without billable provider work.
    Validate {
        /// Emit stable machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Run redacted, non-billable configuration diagnostics.
    Doctor {
        /// Emit stable machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Remove the active provider profile so setup runs again.
''',
    "config doctor command",
)
main = replace_once(
    main,
    '''            Some(ConfigAction::Show { json }) => show_config(json),
            Some(ConfigAction::Get { key, json }) => get_config(&key, json),
''',
    '''            Some(ConfigAction::Show { json }) => show_config(json),
            Some(ConfigAction::Edit) => edit_config(),
            Some(ConfigAction::Get { key, json }) => get_config(&key, json),
''',
    "config edit routing",
)
main = replace_once(
    main,
    '''            Some(ConfigAction::Validate { json }) => validate_config(json),
            Some(ConfigAction::Reset) => reset_config(),
''',
    '''            Some(ConfigAction::Validate { json }) => validate_config(json),
            Some(ConfigAction::Doctor { json }) => doctor_config(json),
            Some(ConfigAction::Reset) => reset_config(),
''',
    "config doctor routing",
)
main_path.write_text(main)

config_path = Path("crates/medusa-cli/src/config_command.rs")
config = config_path.read_text()
config = replace_once(
    config,
    '''use std::{
    io::{self, IsTerminal, Write},
    net::{SocketAddr, TcpStream},
    process::Command,
    time::Duration,
};

use medusa_config::{PROVIDER_PROFILE_KEYS, ProviderProfile, ProviderProfileCatalog};
''',
    '''use std::{
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
    Config, PROVIDER_PROFILE_KEYS, ProviderProfile, ProviderProfileCatalog, ProviderProfileStore,
    credential_environment,
};
''',
    "config command imports",
)
insert = r'''pub(crate) fn edit() -> MedusaResult<()> {
    let store = ProviderProfileCatalog::user()?.active_store()?;
    let previous = store.load()?;
    let existed = store.path().is_file();
    if !existed {
        store.save(&previous)?;
    }

    let status = launch_editor(store.path())?;
    if !status.success() {
        rollback_profile(&store, existed, &previous)?;
        return Err(config_error(format!(
            "configuration editor exited with status {status}; previous configuration restored"
        )));
    }

    let edited = commit_edited_profile(&store, existed, &previous)?;
    println!("Configuration saved to {}", store.path().display());
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
    store: &ProviderProfileStore,
    existed: bool,
    previous: &ProviderProfile,
) -> MedusaResult<ProviderProfile> {
    let candidate = match store.load() {
        Ok(candidate) => candidate,
        Err(_) => {
            rollback_profile(store, existed, previous)?;
            return Err(config_error(
                "edited configuration was invalid; previous configuration restored",
            ));
        }
    };
    if Config::load_layers_with_provider_profile(
        &candidate,
        None,
        None,
        &BTreeMap::new(),
        &BTreeMap::new(),
    )
    .is_err()
    {
        rollback_profile(store, existed, previous)?;
        return Err(config_error(
            "edited configuration was invalid; previous configuration restored",
        ));
    }
    if store.save(&candidate).is_err() {
        rollback_profile(store, existed, previous)?;
        return Err(config_error(
            "edited configuration could not be committed; previous configuration restored",
        ));
    }
    Ok(candidate)
}

fn rollback_profile(
    store: &ProviderProfileStore,
    existed: bool,
    previous: &ProviderProfile,
) -> MedusaResult<()> {
    if existed {
        store.save(previous)
    } else {
        store.reset().map(|_| ())
    }
}

#[derive(Debug, Serialize)]
struct ConfigDoctorOutput {
    healthy: bool,
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
    let store = ProviderProfileCatalog::user()?.active_store()?;
    let report = diagnose_store(&store);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| config_error(error.to_string()))?
        );
        return Ok(());
    }

    println!("Configuration doctor");
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

fn diagnose_store(store: &ProviderProfileStore) -> ConfigDoctorOutput {
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
    add_check(
        &mut checks,
        "profile_store",
        profile_store_writable(store.path()),
        if profile_store_writable(store.path()) {
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
                None,
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
            return finish_doctor(path, false, checks);
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
    finish_doctor(path, profile.configured, checks)
}

fn diagnose_endpoint(profile: &ProviderProfile, checks: &mut Vec<ConfigDoctorCheck>) {
    let Some(base_url) = profile.base_url.as_deref() else {
        add_check(
            checks,
            "endpoint",
            true,
            "selected route uses its provider default endpoint",
            None,
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
            None,
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
    address
        .is_loopback()
        .then(|| SocketAddr::new(address, url.port_or_known_default()?))
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
    configured: bool,
    checks: Vec<ConfigDoctorCheck>,
) -> ConfigDoctorOutput {
    ConfigDoctorOutput {
        healthy: checks.iter().all(|check| check.ok),
        configured,
        path,
        checks,
    }
}

'''
marker = "pub(crate) fn reset() -> MedusaResult<()> {\n"
if insert not in config:
    if config.count(marker) != 1:
        raise SystemExit("config helper insertion target changed")
    config = config.replace(marker, insert + marker, 1)
config = replace_once(
    config,
    '''    fn keyed_values_are_stable_and_redacted() {
        let profile = ProviderProfile::default();
        assert_eq!(
            profile.value("provider"),
            Some(medusa_config::ProviderProfileValue::String(
                "minimax".into()
            ))
        );
        assert!(profile.value("token").is_none());
    }
''',
    '''    fn keyed_values_are_stable_and_redacted() {
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

        let error = commit_edited_profile(&store, true, &previous).expect_err("invalid edit");
        assert!(!error.to_string().contains("super-secret-value"));
        assert_eq!(store.load().expect("restored"), previous);
    }

    #[test]
    fn doctor_report_redacts_malformed_profile_contents() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = ProviderProfileStore::at(directory.path().join("provider.toml"));
        fs::write(store.path(), "token = 'super-secret-value'\n").expect("malformed profile");
        let report = diagnose_store(&store);
        let encoded = serde_json::to_string(&report).expect("doctor json");
        assert!(!report.healthy);
        assert!(!encoded.contains("super-secret-value"));
    }
''',
    "config edit and doctor tests",
)
config_path.write_text(config)

profile_path = Path("crates/medusa-config/src/provider_profile.rs")
profile = profile_path.read_text()
credential_helper = r'''/// Returns the registered environment-variable credential source for a provider.
#[must_use]
pub fn credential_environment(provider: &str) -> Option<&'static str> {
    match provider {
        "minimax" => Some("MINIMAX_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "anthropic-compatible" | "openai-compatible" => Some("MEDUSA_API_KEY"),
        "openai" => Some("OPENAI_API_KEY"),
        "openai-oauth" | "omniroute" | "local" => None,
        _ => None,
    }
}

'''
profile_marker = "#[derive(Clone, Debug, Eq, PartialEq, Serialize)]\n#[serde(untagged)]\npub enum ProviderProfileValue {\n"
if credential_helper not in profile:
    if profile.count(profile_marker) != 1:
        raise SystemExit("credential helper insertion target changed")
    profile = profile.replace(profile_marker, credential_helper + profile_marker, 1)
profile_path.write_text(profile)

lib_path = Path("crates/medusa-config/src/lib.rs")
lib = lib_path.read_text()
lib = replace_once(
    lib,
    '''pub use provider_profile::{
    PROVIDER_PROFILE_KEYS, ProviderProfile, ProviderProfileStore, ProviderProfileValue,
};
''',
    '''pub use provider_profile::{
    PROVIDER_PROFILE_KEYS, ProviderProfile, ProviderProfileStore, ProviderProfileValue,
    credential_environment,
};
''',
    "credential helper export",
)
lib_path.write_text(lib)

support_path = Path("crates/medusa-runtime/src/support.rs")
support = support_path.read_text()
support = replace_once(
    support,
    '''        || credential_environment(&state.config.model.provider)
            .is_some_and(|name| env::var(name).is_ok())
''',
    '''        || medusa_config::credential_environment(&state.config.model.provider)
            .is_some_and(|name| env::var(name).is_ok())
''',
    "shared credential environment usage",
)
support = replace_once(
    support,
    '''pub(super) fn credential_environment(provider: &str) -> Option<&'static str> {
    match provider {
        "minimax" => Some("MINIMAX_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "anthropic-compatible" | "openai-compatible" => Some("MEDUSA_API_KEY"),
        "openai" => Some("OPENAI_API_KEY"),
        "openai-oauth" | "omniroute" | "local" => None,
        _ => None,
    }
}

''',
    '',
    "remove duplicate credential mapping",
)
support_path.write_text(support)
