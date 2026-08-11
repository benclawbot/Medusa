use std::{
    collections::BTreeMap,
    env, fs,
    fs::OpenOptions,
    io::Write,
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use medusa_core::MedusaResult;
use serde::Serialize;

use super::{
    Config, ConfigurationApplyTiming, ConfigurationChangeOrigin, ConfigurationChanged,
    PROVIDER_PROFILE_KEYS, ProviderProfile, ProviderProfileCatalog, ProviderProfileStore,
    credential_environment,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigDoctorStatus {
    Pass,
    Warn,
    ActionRequired,
    OptionalUnavailable,
}

impl ConfigDoctorStatus {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Warn => "WARN",
            Self::ActionRequired => "ACTION REQUIRED",
            Self::OptionalUnavailable => "OPTIONAL/UNAVAILABLE",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigDoctorRepair {
    InitializeProfile,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ConfigDoctorCheck {
    pub name: String,
    pub status: ConfigDoctorStatus,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair: Option<ConfigDoctorRepair>,
}

impl ConfigDoctorCheck {
    #[must_use]
    pub const fn ok(&self) -> bool {
        matches!(
            self.status,
            ConfigDoctorStatus::Pass
                | ConfigDoctorStatus::Warn
                | ConfigDoctorStatus::OptionalUnavailable
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ConfigDoctorReport {
    pub healthy: bool,
    pub revision: u64,
    pub active_profile: String,
    pub configured: bool,
    pub path: String,
    pub checks: Vec<ConfigDoctorCheck>,
}

impl ConfigDoctorReport {
    #[must_use]
    pub fn summary_label(&self) -> &'static str {
        if self
            .checks
            .iter()
            .any(|check| check.status == ConfigDoctorStatus::ActionRequired)
        {
            "action required"
        } else if self
            .checks
            .iter()
            .any(|check| check.status == ConfigDoctorStatus::Warn)
        {
            "warning"
        } else if self
            .checks
            .iter()
            .any(|check| check.status == ConfigDoctorStatus::OptionalUnavailable)
        {
            "optional unavailable"
        } else {
            "healthy"
        }
    }
}

pub fn diagnose_config_catalog(
    catalog: &ProviderProfileCatalog,
) -> MedusaResult<ConfigDoctorReport> {
    let revision = catalog.revision()?;
    let active_profile = catalog.active_name()?;
    let store = catalog.active_store()?;
    Ok(diagnose_store(&store, revision, active_profile))
}

pub fn repair_config_check(
    catalog: &ProviderProfileCatalog,
    check: &ConfigDoctorCheck,
) -> MedusaResult<Option<ConfigurationChanged>> {
    match check.repair {
        Some(ConfigDoctorRepair::InitializeProfile) => {
            let snapshot = catalog.snapshot()?;
            let change = catalog.save_active_profile(
                &snapshot.profile,
                snapshot.revision,
                ConfigurationChangeOrigin::Tui,
                PROVIDER_PROFILE_KEYS.iter().map(|key| (*key).to_owned()),
                ConfigurationApplyTiming::NextSession,
            )?;
            Ok(Some(change))
        }
        None => Ok(None),
    }
}

fn diagnose_store(
    store: &ProviderProfileStore,
    revision: u64,
    active_profile: String,
) -> ConfigDoctorReport {
    let path = store.path().display().to_string();
    let mut checks = Vec::new();
    let exists = store.path().is_file();
    add_check(
        &mut checks,
        "profile_file",
        if exists {
            ConfigDoctorStatus::Pass
        } else {
            ConfigDoctorStatus::ActionRequired
        },
        if exists {
            "active profile exists"
        } else {
            "active profile has not been initialized"
        },
        (!exists).then_some(
            "Run `medusa config init`; in /settings, press Enter on this check to initialize the active non-secret profile.",
        ),
        (!exists).then_some(ConfigDoctorRepair::InitializeProfile),
    );
    let writable = profile_store_writable(store.path());
    add_check(
        &mut checks,
        "profile_store",
        if writable {
            ConfigDoctorStatus::Pass
        } else {
            ConfigDoctorStatus::ActionRequired
        },
        if writable {
            "configuration directory is writable"
        } else {
            "configuration directory is not writable or does not exist"
        },
        (!writable).then_some("Fix the configuration-directory ownership or permissions."),
        None,
    );

    let profile = match store.load() {
        Ok(profile) => {
            add_check(
                &mut checks,
                "schema",
                ConfigDoctorStatus::Pass,
                "profile schema and typed invariants are valid",
                None::<String>,
                None,
            );
            profile
        }
        Err(_) => {
            add_check(
                &mut checks,
                "schema",
                ConfigDoctorStatus::ActionRequired,
                "profile cannot be parsed or validated",
                Some(
                    "Run `medusa config edit` to correct the profile or `medusa config reset` to reset it; the same operations are available from /settings.",
                ),
                None,
            );
            return finish(path, revision, active_profile, false, checks);
        }
    };

    add_check(
        &mut checks,
        "configured",
        if profile.configured {
            ConfigDoctorStatus::Pass
        } else {
            ConfigDoctorStatus::ActionRequired
        },
        if profile.configured {
            "provider profile is configured"
        } else {
            "provider profile is not configured"
        },
        (!profile.configured).then_some(
            "Run `medusa config init` or choose a provider, authentication route, and model in /settings.",
        ),
        None,
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
        if effective {
            ConfigDoctorStatus::Pass
        } else {
            ConfigDoctorStatus::ActionRequired
        },
        if effective {
            "resolved runtime configuration is valid"
        } else {
            "resolved runtime configuration is invalid"
        },
        (!effective).then_some(
            "Run `medusa config validate` and correct the reported setting, or review it in /settings before applying changes.",
        ),
        None,
    );

    diagnose_endpoint(&profile, &mut checks);
    diagnose_credential(&profile, &mut checks);
    finish(path, revision, active_profile, profile.configured, checks)
}

fn diagnose_endpoint(profile: &ProviderProfile, checks: &mut Vec<ConfigDoctorCheck>) {
    let Some(base_url) = profile.base_url.as_deref() else {
        add_check(
            checks,
            "endpoint",
            ConfigDoctorStatus::Pass,
            "selected route uses its provider default endpoint",
            None::<String>,
            None,
        );
        return;
    };
    let parsed = reqwest::Url::parse(base_url);
    add_check(
        checks,
        "endpoint",
        if parsed.is_ok() {
            ConfigDoctorStatus::Pass
        } else {
            ConfigDoctorStatus::ActionRequired
        },
        if parsed.is_ok() {
            "endpoint URL is syntactically valid"
        } else {
            "endpoint URL is invalid"
        },
        parsed.is_err().then_some(
            "Set a valid http:// or https:// provider endpoint with `medusa config edit` or in /settings.",
        ),
        None,
    );
    let Some(address) = loopback_socket(base_url) else {
        return;
    };
    let reachable = TcpStream::connect_timeout(&address, Duration::from_millis(300)).is_ok();
    let remediation = match profile.connection.as_str() {
        "chatgpt-oauth" => {
            "Run `npx openai-oauth@latest login` or use Sign in with browser in /settings, then refresh diagnostics."
        }
        "omniroute" => {
            "Start OmniRoute on the configured loopback endpoint, then refresh diagnostics."
        }
        "local" => "Start the configured local model runtime, then refresh diagnostics.",
        _ => "Start the configured loopback gateway, then refresh diagnostics.",
    };
    add_check(
        checks,
        "local_gateway",
        if reachable {
            ConfigDoctorStatus::Pass
        } else {
            ConfigDoctorStatus::ActionRequired
        },
        if reachable {
            "selected loopback gateway is reachable"
        } else {
            "selected loopback gateway is not reachable"
        },
        (!reachable).then_some(remediation),
        None,
    );
}

fn diagnose_credential(profile: &ProviderProfile, checks: &mut Vec<ConfigDoctorCheck>) {
    if profile.auth != "api-key" {
        add_check(
            checks,
            "credential",
            ConfigDoctorStatus::Pass,
            match profile.auth.as_str() {
                "none" => "selected route does not require a Medusa-managed credential",
                "existing" => "credential ownership is delegated to the selected gateway",
                "oauth" => "credential ownership is delegated to the provider OAuth route",
                _ => "credential mode is valid",
            },
            None::<String>,
            None,
        );
        return;
    }
    let Some(variable) = credential_environment(&profile.provider) else {
        add_check(
            checks,
            "credential",
            ConfigDoctorStatus::ActionRequired,
            "selected provider has no registered API-key environment source",
            Some("Select a supported provider route or use an existing gateway credential."),
            None,
        );
        return;
    };
    let configured = env::var_os(variable).is_some();
    add_check(
        checks,
        "credential",
        if configured {
            ConfigDoctorStatus::Pass
        } else {
            ConfigDoctorStatus::ActionRequired
        },
        if configured {
            format!("{variable} is present; its value was not read or displayed")
        } else {
            format!("{variable} is not present")
        },
        (!configured).then(|| format!("Set {variable} in the Medusa process environment.")),
        None,
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
    name: &str,
    status: ConfigDoctorStatus,
    detail: impl Into<String>,
    remediation: Option<impl Into<String>>,
    repair: Option<ConfigDoctorRepair>,
) {
    checks.push(ConfigDoctorCheck {
        name: name.to_owned(),
        status,
        detail: detail.into(),
        remediation: remediation.map(Into::into),
        repair,
    });
}

fn finish(
    path: String,
    revision: u64,
    active_profile: String,
    configured: bool,
    checks: Vec<ConfigDoctorCheck>,
) -> ConfigDoctorReport {
    ConfigDoctorReport {
        healthy: checks.iter().all(ConfigDoctorCheck::ok),
        revision,
        active_profile,
        configured,
        path,
        checks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_profile_is_actionable_and_repairable_without_credential_values() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = ProviderProfileCatalog::at(directory.path());
        let report = diagnose_config_catalog(&catalog).expect("report");
        let check = report
            .checks
            .iter()
            .find(|check| check.name == "profile_file")
            .expect("profile file check");
        assert_eq!(check.status, ConfigDoctorStatus::ActionRequired);
        assert_eq!(check.repair, Some(ConfigDoctorRepair::InitializeProfile));
        assert!(
            check
                .remediation
                .as_deref()
                .expect("remediation")
                .contains("medusa config init")
        );
        repair_config_check(&catalog, check)
            .expect("repair")
            .expect("change");
        let refreshed = diagnose_config_catalog(&catalog).expect("refreshed");
        assert_eq!(
            refreshed
                .checks
                .iter()
                .find(|check| check.name == "profile_file")
                .expect("check")
                .status,
            ConfigDoctorStatus::Pass
        );
        let serialized = serde_json::to_string(&refreshed).expect("serialize");
        assert!(!serialized.contains("api_key"));
        assert!(!serialized.contains("access_token"));
    }
}
