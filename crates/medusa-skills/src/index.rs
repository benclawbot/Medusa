use std::{
    collections::BTreeMap,
    fs,
    path::Path,
};

use medusa_core::{
    ErrorCategory, ErrorCode, MedusaError, MedusaResult,
};
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
    /// Build an index by walking a runtime assets directory.
    ///
    /// Reads every `assets/skills/<name>/SKILL.md` under `root`, parses its
    /// YAML frontmatter into a `SkillManifest`, and returns the entries
    /// sorted by name. A missing `skills/` subdirectory yields an empty
    /// index.
    pub fn from_assets_dir(root: &Path) -> MedusaResult<Self> {
        let mut skills: Vec<SkillEntry> = Vec::new();
        let skills_root = root.join("skills");
        if !skills_root.is_dir() {
            return Ok(Self::default());
        }
        for dir in fs::read_dir(&skills_root).map_err(|error| {
            MedusaError::new(
                ErrorCode::PersistenceFailed,
                ErrorCategory::Environment,
                format!("read skills dir: {error}"),
            )
        })? {
            let dir = dir
                .map_err(|error| {
                    MedusaError::new(
                        ErrorCode::PersistenceFailed,
                        ErrorCategory::Environment,
                        format!("read dir entry: {error}"),
                    )
                })?
                .path();
            if !dir.is_dir() {
                continue;
            }
            let skill_file = dir.join("SKILL.md");
            if !skill_file.is_file() {
                continue;
            }
            let raw = fs::read_to_string(&skill_file).map_err(|error| {
                MedusaError::new(
                    ErrorCode::PersistenceFailed,
                    ErrorCategory::Environment,
                    format!("read SKILL.md at {}: {error}", skill_file.display()),
                )
            })?;
            let (frontmatter, body) = split_frontmatter(&raw);
            let manifest: SkillManifest = serde_yaml::from_str(frontmatter).map_err(|error| {
                MedusaError::new(
                    ErrorCode::InvalidConfiguration,
                    ErrorCategory::Validation,
                    format!("parse frontmatter at {}: {error}", skill_file.display()),
                )
            })?;
            let requires = manifest.requires.clone();
            let handoff = manifest.handoff.clone();
            skills.push(SkillEntry {
                name: manifest.name.clone(),
                manifest,
                body: body.to_owned(),
                requires,
                handoff,
            });
        }
        skills.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(Self { skills })
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

fn split_frontmatter(text: &str) -> (&str, &str) {
    let trimmed = text.trim_start_matches('\u{feff}');
    let rest = trimmed
        .strip_prefix("---")
        .expect("frontmatter starts with ---");
    let rest = rest.trim_start_matches('\r');
    let (front, body) = rest
        .split_once("\n---")
        .expect("frontmatter closes with ---");
    let body = body.trim_start_matches('\r').trim_start_matches('\n');
    (front, body)
}
