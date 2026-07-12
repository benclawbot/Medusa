use std::{fs, path::PathBuf};

use medusa_core::MedusaResult;

/// Project-local memory engine.
pub struct MemoryEngine {
    pub(crate) root: PathBuf,
    pub(crate) index_path: PathBuf,
}

impl MemoryEngine {
    pub fn new(project_root: impl Into<PathBuf>) -> MedusaResult<Self> {
        let root = project_root.into().join(".medusa/memory");
        fs::create_dir_all(root.join("proposals"))?;
        fs::create_dir_all(root.join("archive"))?;
        fs::create_dir_all(root.join("lessons"))?;
        fs::create_dir_all(root.join("patterns"))?;
        fs::create_dir_all(root.join("failures"))?;
        fs::create_dir_all(root.join("decisions"))?;
        let engine = Self {
            index_path: root.join("index.sqlite3"),
            root,
        };
        engine.initialize_layout()?;
        Ok(engine)
    }
}
