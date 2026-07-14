use std::{
    collections::BTreeMap,
    path::Path,
};

use medusa_core::MedusaResult;
use medusa_extensions::SkillManifest;
use serde::{Deserialize, Serialize};

/// A single entry in the manifest index. Augments `SkillManifest` with
/// `requires` and `handoff` for chained skills.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SkillEntry {
    pub name: String,
    pub manifest: SkillManifest,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub handoff: Option<String>,
}

/// The full index of all skills, as serialized to `manifest.json`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SkillIndex {
    pub skills: Vec<SkillEntry>,
}

impl SkillIndex {
    pub fn from_assets_dir(_root: &Path) -> MedusaResult<Self> {
        Ok(Self::default())
    }

    pub fn entries(&self) -> &[SkillEntry] {
        &self.skills
    }

    pub fn by_name(&self, name: &str) -> Option<&SkillEntry> {
        self.skills.iter().find(|entry| entry.name == name)
    }

    pub fn names_by_name(&self) -> BTreeMap<&str, &SkillEntry> {
        self.skills.iter().map(|entry| (entry.name.as_str(), entry)).collect()
    }
}

/// Re-export of the upstream manifest type so callers don't have to import
/// from two crates.
pub type SkillManifestExt = SkillManifest;
