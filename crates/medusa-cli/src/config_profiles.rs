use medusa_config::{ProviderProfileCatalog, ProviderProfileSummary};
use medusa_core::MedusaResult;

pub(crate) fn set(key: &str, value: &str) -> MedusaResult<()> {
    let catalog = ProviderProfileCatalog::user()?;
    let store = catalog.active_store()?;
    let mut profile = store.load()?;
    profile.set_value(key, value)?;
    store.save(&profile)?;
    println!(
        "Updated `{key}` in provider profile `{}`.",
        catalog.active_name()?
    );
    Ok(())
}

pub(crate) fn unset(key: &str) -> MedusaResult<()> {
    let catalog = ProviderProfileCatalog::user()?;
    let store = catalog.active_store()?;
    let mut profile = store.load()?;
    profile.unset_value(key)?;
    store.save(&profile)?;
    println!(
        "Reset `{key}` to its default in provider profile `{}`.",
        catalog.active_name()?
    );
    Ok(())
}

pub(crate) fn list(json: bool) -> MedusaResult<()> {
    let profiles = ProviderProfileCatalog::user()?.list()?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&profiles)
                .map_err(|error| medusa_core::MedusaError::from(error.to_string()))?
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
    let profile = ProviderProfileCatalog::user()?.create(name)?;
    print_created(&profile);
    Ok(())
}

pub(crate) fn use_profile(name: &str) -> MedusaResult<()> {
    let profile = ProviderProfileCatalog::user()?.use_profile(name)?;
    println!(
        "Active provider profile is now `{}` — {} / {}.",
        profile.name, profile.provider, profile.model
    );
    Ok(())
}

pub(crate) fn delete(name: &str) -> MedusaResult<()> {
    ProviderProfileCatalog::user()?.delete(name)?;
    println!("Deleted provider profile `{name}`.");
    Ok(())
}

fn print_created(profile: &ProviderProfileSummary) {
    println!(
        "Created provider profile `{}` from the current active configuration — {} / {}.",
        profile.name, profile.provider, profile.model
    );
}
