use std::{collections::BTreeSet, fs, path::Path};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> MedusaResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)?;
    Ok(())
}

pub(crate) fn tokenize(value: &str) -> Vec<String> {
    normalize(value)
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

pub(crate) fn normalize(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn sanitize_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect()
}

pub(crate) fn deduplicate(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub(crate) fn first_claim(body: &str) -> String {
    body.lines()
        .find(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .unwrap_or_default()
        .trim()
        .to_owned()
}

pub(crate) fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

pub(crate) fn internal(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        message,
    )
}

pub(crate) fn sql_error(error: rusqlite::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::PersistenceFailed,
        ErrorCategory::Persistence,
        error.to_string(),
    )
}
