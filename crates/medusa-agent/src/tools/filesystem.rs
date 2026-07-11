use std::{
    fs,
    path::{Component, Path},
};

use medusa_core::MedusaResult;
use walkdir::WalkDir;

use crate::policy::safe_path;

pub(crate) fn read(repo: &Path, relative: &str) -> MedusaResult<String> {
    Ok(fs::read_to_string(safe_path(repo, relative)?)?)
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
    Ok(format!("wrote {} bytes to {}", content.len(), path.display()))
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
                    results.push(format!(
                        "{}:{}:{}",
                        entry.path().display(),
                        index + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
    Ok(crate::tools::truncate(results.join("\n")))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{read, search, write};

    #[test]
    fn extracted_filesystem_tools_preserve_read_write_and_search_behavior() {
        let directory = tempfile::tempdir().expect("tempdir");
        let receipt = write(directory.path(), "nested/value.txt", "alpha\nbeta\n")
            .expect("atomic write");
        assert!(receipt.contains("11 bytes"));
        assert_eq!(
            read(directory.path(), "nested/value.txt").expect("read"),
            "alpha\nbeta\n"
        );

        fs::create_dir_all(directory.path().join(".medusa")).expect("medusa dir");
        fs::write(directory.path().join(".medusa/hidden.txt"), "alpha")
            .expect("hidden fixture");
        let matches = search(directory.path(), "beta").expect("search");
        assert!(matches.contains("nested/value.txt:2:beta"));
        assert!(!matches.contains("hidden.txt"));
    }

    #[test]
    fn extracted_filesystem_tools_reject_parent_traversal() {
        let directory = tempfile::tempdir().expect("tempdir");
        assert!(read(directory.path(), "../secret.txt").is_err());
        assert!(write(directory.path(), "../secret.txt", "nope").is_err());
    }
}
