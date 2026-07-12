use std::{fs, path::{Path, PathBuf}};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::Serialize;
use sha2::{Digest, Sha256};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

pub(crate) fn append_atomic(path: &Path, bytes: &[u8]) -> MedusaResult<()> {
    let mut existing = if path.exists() { fs::read(path)? } else { Vec::new() };
    existing.extend_from_slice(bytes);
    atomic_write(path, &existing)
}

pub(crate) fn copy_tree(source: &Path, destination: &Path, skip_name: Option<&str>) -> MedusaResult<()> {
    fs::create_dir_all(destination)?;
    if !source.exists() {
        return Ok(());
    }
    let mut entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        if skip_name.is_some_and(|name| entry.file_name() == name) {
            continue;
        }
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_path)?;
        if metadata.file_type().is_symlink() {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                format!("state tree contains symlink: {}", source_path.display()),
            ));
        }
        if metadata.is_dir() {
            copy_tree(&source_path, &destination_path, None)?;
        } else if metadata.is_file() {
            fs::copy(source_path, destination_path)?;
        }
    }
    Ok(())
}

pub(crate) fn restore_tree(backup: &Path, root: &Path) -> MedusaResult<()> {
    let preserved_backups = root.join("backups");
    for entry in fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()? {
        if entry.path() == preserved_backups {
            continue;
        }
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    copy_tree(backup, root, None)
}

pub(crate) fn directory_digest(root: &Path) -> MedusaResult<String> {
    if !root.exists() {
        return Ok(format!("{:x}", Sha256::digest([])));
    }
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = Sha256::new();
    for (relative, bytes) in files {
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update([0]);
        hasher.update(bytes);
        hasher.update([0]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn collect_files(root: &Path, current: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) -> MedusaResult<()> {
    for entry in fs::read_dir(current)?.collect::<Result<Vec<_>, _>>()? {
        let path = entry.path();
        if path.components().any(|component| component.as_os_str() == "backups") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                format!("digest tree contains symlink: {}", path.display()),
            ));
        }
        if metadata.is_dir() {
            collect_files(root, &path, files)?;
        } else if metadata.is_file() {
            files.push((path.strip_prefix(root).unwrap_or(&path).to_path_buf(), fs::read(path)?));
        }
    }
    Ok(())
}

pub(crate) fn atomic_json(path: &Path, value: &impl Serialize) -> MedusaResult<()> {
    atomic_write(path, &serde_json::to_vec_pretty(value)?)
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> MedusaResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)?;
    Ok(())
}

pub(crate) fn now() -> MedusaResult<String> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| internal(error.to_string()))
}

pub(crate) fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::InvalidConfiguration, ErrorCategory::Validation, message)
}

pub(crate) fn internal(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::InternalInvariant, ErrorCategory::Internal, message)
}
