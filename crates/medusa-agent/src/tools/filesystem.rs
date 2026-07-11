use std::{fs, path::{Component, Path}};

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
