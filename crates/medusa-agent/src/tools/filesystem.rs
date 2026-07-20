use std::{
    fs,
    path::{Component, Path},
};

use medusa_core::MedusaResult;
use walkdir::WalkDir;

use crate::policy::safe_path;

pub(crate) fn read(repo: &Path, relative: &str) -> MedusaResult<String> {
    if relative == "." {
        return Ok(repository_listing(repo));
    }
    Ok(fs::read_to_string(safe_path(repo, relative)?)?)
}

fn repository_listing(repo: &Path) -> String {
    const MAX_ENTRIES: usize = 80;
    let mut entries = WalkDir::new(repo)
        .min_depth(1)
        .max_depth(2)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            !entry.path().components().any(|part| {
                matches!(part, Component::Normal(name) if name == ".git" || name == ".medusa")
            })
        })
        .filter_map(|entry| {
            let relative = entry.path().strip_prefix(repo).ok()?;
            let mut display = relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            if entry.file_type().is_dir() {
                display.push('/');
            }
            Some(display)
        })
        .take(MAX_ENTRIES)
        .collect::<Vec<_>>();
    entries.sort();
    if entries.len() == MAX_ENTRIES {
        entries.push("... listing truncated".to_owned());
    }
    entries.join("\n")
}

pub(crate) fn write(repo: &Path, relative: &str, content: &str) -> MedusaResult<String> {
    let path = safe_path(repo, relative)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let original_permissions = fs::metadata(&path)
        .ok()
        .map(|metadata| metadata.permissions());
    let temporary = path.with_extension("medusa-tmp");
    fs::write(&temporary, content)?;
    if let Some(permissions) = original_permissions {
        fs::set_permissions(&temporary, permissions)?;
    }
    fs::rename(&temporary, &path)?;
    Ok(format!(
        "wrote {} bytes to {}",
        content.len(),
        path.display()
    ))
}

pub(crate) fn create_dir(repo: &Path, relative: &str) -> MedusaResult<String> {
    let path = safe_path(repo, relative)?;
    fs::create_dir_all(&path)?;
    Ok(format!("created directory {}", path.display()))
}

pub(crate) fn write_approved(path: &str, content: &str) -> MedusaResult<String> {
    let path = approved_absolute_path(path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, content)?;
    Ok(format!(
        "wrote {} bytes to {}",
        content.len(),
        path.display()
    ))
}

pub(crate) fn create_dir_approved(path: &str) -> MedusaResult<String> {
    let path = approved_absolute_path(path)?;
    fs::create_dir_all(&path)?;
    Ok(format!("created directory {}", path.display()))
}

fn approved_absolute_path(value: &str) -> MedusaResult<std::path::PathBuf> {
    use medusa_core::{ErrorCategory, ErrorCode, MedusaError};

    let path = Path::new(value);
    if !path.is_absolute() || path.parent().is_none() {
        return Err(MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            "an approved external path must be absolute and narrower than a filesystem root",
        ));
    }
    let mut existing = path;
    while !existing.exists() {
        existing = existing.parent().ok_or_else(|| {
            MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                "approved path has no existing confined ancestor",
            )
        })?;
    }
    let canonical_existing = existing.canonicalize()?;
    let suffix = path.strip_prefix(existing).map_err(|error| {
        MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            format!("approved path could not be confined: {error}"),
        )
    })?;
    let resolved = canonical_existing.join(suffix);
    if resolved.exists() && fs::symlink_metadata(&resolved)?.file_type().is_symlink() {
        return Err(MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            "approved path targets a symbolic link",
        ));
    }
    Ok(resolved)
}

pub(crate) fn search(repo: &Path, query: &str) -> MedusaResult<String> {
    let mut results = Vec::new();
    for entry in WalkDir::new(repo).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file()
            || entry.path().components().any(|part| {
                matches!(part, Component::Normal(name) if name == ".git" || name == ".medusa")
            })
        {
            continue;
        }
        if let Ok(text) = fs::read_to_string(entry.path()) {
            for (index, line) in text.lines().enumerate() {
                if line.contains(query) {
                    let relative = entry.path().strip_prefix(repo).unwrap_or(entry.path());
                    let relative = relative
                        .components()
                        .map(|part| part.as_os_str().to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/");
                    results.push(format!("{}:{}:{}", relative, index + 1, line.trim()));
                }
            }
        }
    }
    Ok(results.join("\n"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{create_dir, read, search, write};

    #[test]
    fn extracted_filesystem_tools_preserve_read_write_and_search_behavior() {
        let directory = tempfile::tempdir().expect("tempdir");
        let directory_receipt =
            create_dir(directory.path(), "nested/assets").expect("create nested directory");
        assert!(directory_receipt.contains("nested"));
        assert!(directory.path().join("nested/assets").is_dir());
        let receipt =
            write(directory.path(), "nested/value.txt", "alpha\nbeta\n").expect("atomic write");
        assert!(receipt.contains("11 bytes"));
        assert_eq!(
            read(directory.path(), "nested/value.txt").expect("read"),
            "alpha\nbeta\n"
        );

        let listing = read(directory.path(), ".").expect("repository listing");
        assert!(listing.contains("nested/"));
        assert!(listing.contains("nested/value.txt"));

        fs::create_dir_all(directory.path().join(".medusa")).expect("medusa dir");
        fs::write(directory.path().join(".medusa/hidden.txt"), "alpha").expect("hidden fixture");
        let matches = search(directory.path(), "beta").expect("search");
        assert!(matches.contains("nested/value.txt:2:beta"));
        assert!(!matches.contains("hidden.txt"));
    }

    #[test]
    fn extracted_filesystem_tools_reject_parent_traversal() {
        let directory = tempfile::tempdir().expect("tempdir");
        assert!(read(directory.path(), "../secret.txt").is_err());
        assert!(write(directory.path(), "../secret.txt", "nope").is_err());
        assert!(create_dir(directory.path(), "../outside").is_err());
    }
}
