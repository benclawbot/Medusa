use std::{
    collections::BTreeMap,
    io::{self, IsTerminal},
};

use medusa_config::{
    Config, ConfigurationApplyTiming, ConfigurationChangeOrigin, PROVIDER_PROFILE_KEYS,
    ProviderProfile, ProviderProfileCatalog,
};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_tui::setup::{
    BrowserOAuthSession, ExistingProfileChoice, FirstRunSetupHost, FirstRunSetupOutcome,
    FirstRunSetupRequest, run_first_run_setup_with_host,
};

use crate::oauth_preflight;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FirstRunDisposition {
    Continue,
    Cancelled,
}

pub(crate) fn ensure_first_run() -> MedusaResult<FirstRunDisposition> {
    run_setup(true)
}

pub(crate) fn configure_interactive() -> MedusaResult<FirstRunDisposition> {
    run_setup(false)
}

fn run_setup(skip_configured: bool) -> MedusaResult<FirstRunDisposition> {
    let catalog = ProviderProfileCatalog::user()?;
    let snapshot = catalog.snapshot()?;
    if skip_configured && snapshot.profile.configured {
        return Ok(FirstRunDisposition::Continue);
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return if skip_configured {
            Ok(FirstRunDisposition::Continue)
        } else {
            Err(config_error(
                "`medusa config init` requires an interactive terminal for native provider setup",
            ))
        };
    }

    let existing_profiles = catalog
        .list()?
        .into_iter()
        .filter(|profile| profile.configured && profile.name != snapshot.active_profile)
        .map(|profile| ExistingProfileChoice {
            name: profile.name,
            provider: profile.provider,
            model: profile.model,
        })
        .collect();
    let mut host = CliSetupHost;
    let outcome = run_first_run_setup_with_host(
        FirstRunSetupRequest {
            initial_profile: snapshot.profile,
            existing_profiles,
        },
        &mut host,
    )
    .map_err(|error| config_error(format!("provider setup failed: {error}")))?;

    match outcome {
        FirstRunSetupOutcome::Cancelled => Ok(FirstRunDisposition::Cancelled),
        FirstRunSetupOutcome::Configure(profile) => {
            let config = validate_candidate(&profile)?;
            oauth_preflight::run_if_needed(&config)?;
            catalog.save_active_profile(
                &profile,
                snapshot.revision,
                ConfigurationChangeOrigin::Tui,
                PROVIDER_PROFILE_KEYS.iter().map(|key| (*key).to_owned()),
                ConfigurationApplyTiming::NextSession,
            )?;
            Ok(FirstRunDisposition::Continue)
        }
        FirstRunSetupOutcome::UseExisting(name) => {
            let profile = catalog.load_profile(&name)?;
            if !profile.configured {
                return Err(config_error(format!(
                    "provider profile `{name}` is not configured"
                )));
            }
            let config = validate_candidate(&profile)?;
            oauth_preflight::run_if_needed(&config)?;
            catalog.use_profile_at_revision(
                &name,
                snapshot.revision,
                ConfigurationChangeOrigin::Tui,
            )?;
            Ok(FirstRunDisposition::Continue)
        }
    }
}

struct CliSetupHost;

impl FirstRunSetupHost for CliSetupHost {
    fn start_browser_oauth(
        &mut self,
        provider_id: &str,
    ) -> Result<Box<dyn BrowserOAuthSession>, String> {
        if provider_id != "openai-oauth" {
            return Err(format!(
                "provider `{provider_id}` does not expose a Medusa browser sign-in helper"
            ));
        }
        let login = medusa_runtime::start_openai_oauth_login().map_err(|error| {
            format!(
                "could not launch browser sign-in through Codex app-server: {error}. Install the Codex CLI and retry from Medusa"
            )
        })?;
        Ok(Box::new(OpenAiOAuthLogin { login }))
    }
}

struct OpenAiOAuthLogin {
    login: medusa_runtime::OpenAiOAuthLogin,
}

impl BrowserOAuthSession for OpenAiOAuthLogin {
    fn poll(&mut self) -> io::Result<Option<Result<Vec<String>, String>>> {
        Ok(self.login.poll())
    }

    fn cancel(&mut self) {
        self.login.cancel();
    }
}

fn validate_candidate(profile: &ProviderProfile) -> MedusaResult<Config> {
    profile.validate()?;
    Config::load_layers_with_provider_profile(
        profile,
        None,
        None,
        &BTreeMap::new(),
        &BTreeMap::new(),
    )
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
    fn configured_candidates_pass_the_existing_config_loader() {
        let profile = ProviderProfile {
            configured: true,
            ..ProviderProfile::default()
        };
        validate_candidate(&profile).expect("candidate");
    }

    #[test]
    fn invalid_candidates_fail_before_catalog_mutation() {
        let profile = ProviderProfile {
            configured: true,
            provider: String::new(),
            ..ProviderProfile::default()
        };
        assert!(validate_candidate(&profile).is_err());
    }

    #[test]
    fn non_oauth_provider_is_rejected_by_browser_host() {
        let mut host = CliSetupHost;
        assert!(host.start_browser_oauth("minimax").is_err());
    }
}
