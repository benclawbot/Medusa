use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use sha2::{Digest, Sha256};

pub(crate) fn split_frontmatter(text: &str) -> MedusaResult<(&str, &str)> {
    let rest = text
        .strip_prefix("---\n")
        .ok_or_else(|| invalid("SKILL.md is missing frontmatter"))?;
    rest.split_once("\n---\n")
        .ok_or_else(|| invalid("SKILL.md frontmatter is not terminated"))
}

pub(crate) fn directory_digest(root: &Path) -> MedusaResult<String> {
    let mut hasher = Sha256::new();
    for path in walk_files(root)? {
        let relative = path
            .strip_prefix(root)
            .map_err(|_| internal("skill path escaped root"))?;
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update([0]);
        hasher.update(fs::read(path)?);
        hasher.update([0]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

pub(crate) fn file_digest(path: &Path) -> MedusaResult<String> {
    Ok(format!("sha256:{:x}", Sha256::digest(fs::read(path)?)))
}

pub(crate) fn walk_files(root: &Path) -> MedusaResult<Vec<PathBuf>> {
    fn visit(directory: &Path, files: &mut Vec<PathBuf>) -> MedusaResult<()> {
        let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(MedusaError::new(
                    ErrorCode::PolicyDenied,
                    ErrorCategory::Policy,
                    format!("extension tree contains symlink: {}", path.display()),
                ));
            }
            if metadata.is_dir() {
                visit(&path, files)?;
            } else if metadata.is_file() {
                files.push(path);
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(root, &mut files)?;
    Ok(files)
}

pub(crate) fn validate_relative_tree(root: &Path) -> MedusaResult<()> {
    if !root.is_dir() {
        return Err(invalid(format!(
            "extension root is not a directory: {}",
            root.display()
        )));
    }
    Ok(())
}

pub(crate) fn wait_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> MedusaResult<std::process::Output> {
    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output().map_err(Into::into);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(MedusaError::new(
                ErrorCode::ToolExecutionFailed,
                ErrorCategory::Execution,
                format!("subprocess exceeded timeout of {} ms", timeout.as_millis()),
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
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

pub(crate) fn yaml_error(error: serde_yaml::Error) -> MedusaError {
    invalid(error.to_string())
}
