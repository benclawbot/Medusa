use medusa_config::{
    ConfigurationApplyTiming, ConfigurationChangeOrigin, ProviderProfileCatalog,
    ProviderProfileSection, ProviderProfileSummary,
};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

pub(crate) fn set(key: &str, value: &str) -> MedusaResult<()> {
    let catalog = ProviderProfileCatalog::user()?;
    let mut staged = catalog.stage_active_profile()?;
    staged.set(key, value)?;
    let change = staged.commit(
        &catalog,
        ConfigurationChangeOrigin::Cli,
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
    let mut staged = catalog.stage_active_profile()?;
    staged.unset(key)?;
    let change = staged.commit(
        &catalog,
        ConfigurationChangeOrigin::Cli,
        ConfigurationApplyTiming::NextSession,
    )?;
    println!(
        "Reset `{key}` to its default in provider profile `{}` at revision {}.",
        change.active_profile, change.revision
    );
    Ok(())
}

pub(crate) fn reset_section(section: &str) -> MedusaResult<()> {
    let catalog = ProviderProfileCatalog::user()?;
    let mut staged = catalog.stage_active_profile()?;
    let section = match section {
        "connection" => ProviderProfileSection::Connection,
        "preferences" => ProviderProfileSection::Preferences,
        _ => return Err(cli_error("section must be `connection` or `preferences`")),
    };
    staged.reset_section(section)?;
    let review = staged.render_diff();
    if review.is_empty() {
        println!("Configuration section is already at defaults.");
        return Ok(());
    }
    println!("Configuration changes:\n{review}");
    let change = staged.commit(
        &catalog,
        ConfigurationChangeOrigin::Cli,
        ConfigurationApplyTiming::NextSession,
    )?;
    println!("Reset section `{section:?}` at revision {}.", change.revision);
    Ok(())
}

pub(crate) fn history(json: bool) -> MedusaResult<()> {
    let history = ProviderProfileCatalog::user()?.active_profile_history()?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&history).map_err(|error| cli_error(error.to_string()))?
        );
    } else if history.is_empty() {
        println!("No previous known-good provider profiles are retained.");
    } else {
        for entry in history {
            println!(
                "revision {} · {} · {} / {}",
                entry.revision, entry.active_profile, entry.profile.provider, entry.profile.model
            );
        }
    }
    Ok(())
}

pub(crate) fn rollback() -> MedusaResult<()> {
    let catalog = ProviderProfileCatalog::user()?;
    let revision = catalog.revision()?;
    let change = catalog.restore_previous_active_profile(
        revision,
        ConfigurationChangeOrigin::Cli,
    )?;
    println!(
        "Restored previous known-good provider profile at revision {}.",
        change.revision
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
