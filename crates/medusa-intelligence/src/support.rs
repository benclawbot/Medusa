use std::path::{Component, Path, PathBuf};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

pub(crate) fn source_files(repo: &Path) -> Vec<PathBuf> {
    let mut paths = WalkDir::new(repo)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| matches!(extension, "rs" | "py"))
        })
        .filter(|path| !path.components().any(is_ignored_component))
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn is_ignored_component(component: Component<'_>) -> bool {
    matches!(
        component,
        Component::Normal(name)
            if name == ".git"
                || name == "target"
                || name == ".medusa"
                || name == ".venv"
                || name == "venv"
                || name == "__pycache__"
                || name == "vendor"
                || name == "node_modules"
                || name == "build"
                || name == "dist"
                || name == "generated"
    )
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
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn discovery_excludes_generated_vendor_and_environment_trees() {
        let repository = tempfile::tempdir().expect("repository");
        fs::create_dir_all(repository.path().join("src")).expect("src");
        fs::write(repository.path().join("src/lib.rs"), "fn included() {}\n").expect("source");

        for directory in [
            "target",
            "vendor",
            "node_modules",
            "build",
            "dist",
            "generated",
            ".venv",
            "venv",
            "__pycache__",
            ".medusa",
            ".git",
        ] {
            fs::create_dir_all(repository.path().join(directory)).expect("ignored directory");
            fs::write(
                repository.path().join(directory).join("ignored.rs"),
                "fn ignored() {}\n",
            )
            .expect("ignored source");
        }

        assert_eq!(
            source_files(repository.path()),
            vec![repository.path().join("src/lib.rs")]
        );
    }
}
