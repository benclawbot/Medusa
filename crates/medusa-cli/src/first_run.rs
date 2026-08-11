use std::{collections::BTreeMap, io::IsTerminal};

use medusa_config::{
    Config, ConfigurationApplyTiming, ConfigurationChangeOrigin, PROVIDER_PROFILE_KEYS,
    ProviderProfile, ProviderProfileCatalog,
};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_tui::setup::{
    ExistingProfileChoice, FirstRunSetupOutcome, FirstRunSetupRequest, run_first_run_setup,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FirstRunDisposition {
    Continue,
    Cancelled,
}

pub(crate) fn ensure_first_run() -> MedusaResult<FirstRunDisposition> {
    let catalog = ProviderProfileCatalog::user()?;
    let snapshot = catalog.snapshot()?;
    if snapshot.profile.configured
        || !std::io::stdin().is_terminal()
        || !std::io::stdout().is_terminal()
    {
        return Ok(FirstRunDisposition::Continue);
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
    let outcome = run_first_run_setup(FirstRunSetupRequest {
        initial_profile: snapshot.profile,
        existing_profiles,
    })
    .map_err(|error| config_error(format!("first-run terminal setup failed: {error}")))?;

    match outcome {
        FirstRunSetupOutcome::Cancelled => Ok(FirstRunDisposition::Cancelled),
        FirstRunSetupOutcome::Configure(profile) => {
            validate_candidate(&profile)?;
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
            validate_candidate(&profile)?;
            catalog.use_profile_at_revision(
                &name,
                snapshot.revision,
                ConfigurationChangeOrigin::Tui,
            )?;
            Ok(FirstRunDisposition::Continue)
        }
    }
}

fn validate_candidate(profile: &ProviderProfile) -> MedusaResult<()> {
    profile.validate()?;
    Config::load_layers_with_provider_profile(
        profile,
        None,
        None,
        &BTreeMap::new(),
        &BTreeMap::new(),
    )
    .map(|_| ())
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
}
