use medusa_config::{
    ConfigurationApplyTiming, ConfigurationChangeOrigin, ProviderProfileCatalog,
    ProviderProfileSummary,
};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

pub(crate) fn set(key: &str, value: &str) -> MedusaResult<()> {
    let catalog = ProviderProfileCatalog::user()?;
    let snapshot = catalog.snapshot()?;
    let mut profile = snapshot.profile;
    profile.set_value(key, value)?;
    let change = catalog.save_active_profile(
        &profile,
        snapshot.revision,
        ConfigurationChangeOrigin::Cli,
        [key.to_owned()],
        ConfigurationApplyTiming::NextSession,
    )?;
    println!(
        "Updated `{key}` in provider profile `{}` at revision {}.",
        change.active_profile, change.revision
    );
    Ok(())
}

pub(crate) fn unset(key: &str) -> MedusaResult<()> {
    let catalog = ProviderProfileCatalog::user()?;
    let snapshot = catalog.snapshot()?;
    let mut profile = snapshot.profile;
    profile.unset_value(key)?;
    let change = catalog.save_active_profile(
        &profile,
        snapshot.revision,
        ConfigurationChangeOrigin::Cli,
        [key.to_owned()],
        ConfigurationApplyTiming::NextSession,
    )?;
    println!(
        "Reset `{key}` to its default in provider profile `{}` at revision {}.",
        change.active_profile, change.revision
    );
    Ok(())
}

pub(crate) fn list(json: bool) -> MedusaResult<()> {
    let profiles = ProviderProfileCatalog::user()?.list()?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&profiles)
                .map_err(|error| cli_error(error.to_string()))?
        );
        return Ok(());
    }
    for profile in profiles {
        let marker = if profile.active { "*" } else { " " };
        let configured = if profile.configured {
            "configured"
        } else {
            "not configured"
        };
        println!(
            "[{marker}] {} — {} / {} ({configured})",
            profile.name, profile.provider, profile.model
        );
    }
    Ok(())
}

pub(crate) fn create(name: &str) -> MedusaResult<()> {
    let catalog = ProviderProfileCatalog::user()?;
    let revision = catalog.revision()?;
    let (profile, change) = catalog.create_at_revision(
        name,
        revision,
        ConfigurationChangeOrigin::Cli,
    )?;
    print_created(&profile);
    println!("Configuration revision: {}.", change.revision);
    Ok(())
}

pub(crate) fn use_profile(name: &str) -> MedusaResult<()> {
    let catalog = ProviderProfileCatalog::user()?;
    let revision = catalog.revision()?;
    let (profile, change) = catalog.use_profile_at_revision(
        name,
        revision,
        ConfigurationChangeOrigin::Cli,
    )?;
    println!(
        "Active provider profile is now `{}` — {} / {} (revision {}).",
        profile.name, profile.provider, profile.model, change.revision
    );
    Ok(())
}

pub(crate) fn delete(name: &str) -> MedusaResult<()> {
    let catalog = ProviderProfileCatalog::user()?;
    let revision = catalog.revision()?;
    let change = catalog.delete_at_revision(
        name,
        revision,
        ConfigurationChangeOrigin::Cli,
    )?;
    println!(
        "Deleted provider profile `{name}` at revision {}.",
        change.revision
    );
    Ok(())
}

fn print_created(profile: &ProviderProfileSummary) {
    println!(
        "Created provider profile `{}` from the current active configuration — {} / {}.",
        profile.name, profile.provider, profile.model
    );
}

fn cli_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}
