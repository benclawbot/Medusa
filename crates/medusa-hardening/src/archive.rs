use std::{collections::BTreeSet, path::{Component, Path}};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

/// Validates archive entry paths without extracting them.
pub fn validate_archive_entries<'a>(
    entries: impl IntoIterator<Item = &'a str>,
) -> MedusaResult<()> {
    let mut seen = BTreeSet::new();
    for entry in entries {
        let path = Path::new(entry);
        if entry.is_empty()
            || path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
            || !seen.insert(path.to_path_buf())
        {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                format!("unsafe archive entry: {entry}"),
            ));
        }
    }
    Ok(())
}
