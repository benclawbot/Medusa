use std::{
    fs,
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

use crate::index::SkillIndex;

pub const MANIFEST_FILENAME: &str = "manifest.json";

/// Reads the generated manifest from a directory.
pub struct AssetStore {
    pub manifest_path: PathBuf,
}

impl AssetStore {
    pub fn load(manifest_path: &Path) -> MedusaResult<Self> {
        if !manifest_path.is_file() {
            return Err(MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Transient,
                format!("manifest not found at {}", manifest_path.display()),
            ));
        }
        Ok(Self { manifest_path: manifest_path.to_path_buf() })
    }

    pub fn index(&self) -> MedusaResult<SkillIndex> {
        let bytes = fs::read(&self.manifest_path).map_err(|e| io_err("read manifest", e))?;
        let index: SkillIndex = serde_json::from_slice(&bytes)
            .map_err(|e| MedusaError::new(ErrorCode::InvalidConfiguration, ErrorCategory::Validation, format!("parse manifest: {e}")))?;
        Ok(index)
    }
}

fn io_err(ctx: &str, e: std::io::Error) -> MedusaError {
    MedusaError::new(ErrorCode::PersistenceFailed, ErrorCategory::Environment, format!("{ctx}: {e}"))
}
