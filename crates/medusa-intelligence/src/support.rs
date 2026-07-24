use std::path::{Path, PathBuf};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use sha2::{Digest, Sha256};

use crate::discovery::RepositorySnapshot;

pub(crate) fn source_files(repo: &Path) -> MedusaResult<Vec<PathBuf>> {
    Ok(RepositorySnapshot::scan(repo)?
        .paths_with_extension("rs")
        .into_iter()
        .map(|path| repo.join(path))
        .collect())
}

pub(crate) fn valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

pub(crate) fn validate_relative(path: &Path) -> MedusaResult<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            format!("path escapes repository: {}", path.display()),
        ));
    }
    Ok(())
}

pub(crate) fn relative(repo: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(repo).unwrap_or(path).to_path_buf()
}

pub(crate) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
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
