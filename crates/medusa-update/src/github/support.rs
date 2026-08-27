//! Bounded GitHub wire-format and persistence helpers for the verified updater.

use std::{io::Read, path::Path};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use reqwest::blocking::Response;
use serde::Deserialize;

use crate::model::invalid;

#[derive(Deserialize)]
pub(super) struct GithubRelease {
    pub(super) tag_name: String,
    pub(super) draft: bool,
    pub(super) prerelease: bool,
    pub(super) assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
pub(super) struct GithubAsset {
    pub(super) name: String,
    pub(super) browser_download_url: String,
    pub(super) size: u64,
}

pub(super) fn read_bounded(
    mut response: Response,
    maximum: usize,
    label: &str,
) -> MedusaResult<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(invalid(format!("{label} exceeds {maximum} bytes")));
    }
    let mut body = Vec::new();
    response
        .by_ref()
        .take(maximum as u64 + 1)
        .read_to_end(&mut body)
        .map_err(http_error)?;
    if body.len() > maximum {
        return Err(invalid(format!("{label} exceeds {maximum} bytes")));
    }
    Ok(body)
}

pub(super) fn sync_parent(path: &Path) -> MedusaResult<()> {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub(super) fn http_error(error: impl std::fmt::Display) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Transient,
        format!("GitHub release request failed: {error}"),
    )
}
