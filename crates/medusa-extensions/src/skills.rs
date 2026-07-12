use std::{
    fs,
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};

use crate::support::{
    directory_digest, invalid, split_frontmatter, validate_relative_tree, walk_files, yaml_error,
};

/// Parsed and audited skill metadata.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SkillManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    pub permissions: SkillPermissions,
    pub compatibility: SkillCompatibility,
    #[serde(default)]
    pub tests: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SkillPermissions {
    #[serde(default)]
    pub network: String,
    #[serde(default)]
    pub write_paths: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SkillCompatibility {
    pub medusa: String,
}

/// Loaded skill with immutable provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedSkill {
    pub manifest: SkillManifest,
    pub body: String,
    pub digest: String,
    pub origin: String,
    pub root: PathBuf,
}

/// Loads and statically validates a pinned skill directory.
pub fn load_skill(root: &Path, origin: &str, expected_digest: &str) -> MedusaResult<LoadedSkill> {
    validate_relative_tree(root)?;
    let skill_path = root.join("SKILL.md");
    let text = fs::read_to_string(&skill_path)?;
    let (frontmatter, body) = split_frontmatter(&text)?;
    let manifest: SkillManifest = serde_yaml::from_str(frontmatter).map_err(yaml_error)?;
    validate_skill_manifest(&manifest)?;
    static_skill_scan(root)?;
    let digest = directory_digest(root)?;
    if digest != expected_digest {
        return Err(MedusaError::new(
            ErrorCode::ChecksumMismatch,
            ErrorCategory::Validation,
            format!("skill digest mismatch: expected {expected_digest}, got {digest}"),
        ));
    }
    Ok(LoadedSkill {
        manifest,
        body: body.to_owned(),
        digest,
        origin: origin.to_owned(),
        root: root.to_path_buf(),
    })
}

fn validate_skill_manifest(manifest: &SkillManifest) -> MedusaResult<()> {
    if manifest.name.trim().is_empty()
        || manifest.version.trim().is_empty()
        || manifest.description.trim().is_empty()
        || manifest.compatibility.medusa.trim().is_empty()
    {
        return Err(invalid("skill metadata is incomplete"));
    }
    if !manifest.name.chars().all(|character| {
        character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
    }) {
        return Err(invalid("skill name must be lowercase kebab-case"));
    }
    if manifest.tools.iter().any(|tool| tool.trim().is_empty()) {
        return Err(invalid("skill tool names cannot be empty"));
    }
    Ok(())
}

fn static_skill_scan(root: &Path) -> MedusaResult<()> {
    for entry in walk_files(root)? {
        let bytes = fs::read(&entry)?;
        let text = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
        for forbidden in [
            "ignore previous instructions",
            "disable policy",
            "print all environment variables",
            "cat ~/.ssh",
            "curl | sh",
        ] {
            if text.contains(forbidden) {
                return Err(MedusaError::new(
                    ErrorCode::PolicyDenied,
                    ErrorCategory::Policy,
                    format!(
                        "skill static scan rejected {}: {forbidden}",
                        entry.display()
                    ),
                ));
            }
        }
    }
    Ok(())
}
