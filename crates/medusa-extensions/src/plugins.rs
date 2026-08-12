use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};

use crate::{
    skills::load_skill,
    support::{directory_digest, invalid},
};

pub const PLUGIN_MANIFEST_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginKind {
    InstructionOnly,
    Tool,
    Mcp,
    Composite,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginAuthentication {
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub methods: BTreeSet<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginPermissions {
    #[serde(default)]
    pub read_paths: BTreeSet<String>,
    #[serde(default)]
    pub write_paths: BTreeSet<String>,
    #[serde(default)]
    pub network_hosts: BTreeSet<String>,
    #[serde(default)]
    pub environment: BTreeSet<String>,
    #[serde(default)]
    pub process_spawn: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginIntegrity {
    pub algorithm: String,
    pub digest: String,
    pub origin: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedPluginManifest {
    pub schema_version: u16,
    pub id: String,
    pub version: String,
    pub kind: PluginKind,
    pub description: String,
    #[serde(default)]
    pub instructions: Vec<String>,
    #[serde(default)]
    pub required_capabilities: BTreeSet<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub scripts: Vec<String>,
    #[serde(default)]
    pub resources: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    #[serde(default)]
    pub authentication: PluginAuthentication,
    #[serde(default)]
    pub permissions: PluginPermissions,
    pub compatibility: String,
    pub integrity: PluginIntegrity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedPlugin {
    pub manifest: ManagedPluginManifest,
    pub root: PathBuf,
    pub instruction_body: Option<String>,
}

pub fn load_managed_plugin(
    root: &Path,
    origin: &str,
    expected_digest: &str,
) -> MedusaResult<LoadedPlugin> {
    let manifest_path = root.join("plugin.json");
    if manifest_path.is_file() {
        let text = fs::read_to_string(&manifest_path)?;
        let mut manifest: ManagedPluginManifest =
            serde_json::from_str(&text).map_err(json_error)?;
        let digest = directory_digest(root)?;
        if digest != expected_digest {
            return Err(checksum_error(expected_digest, &digest));
        }
        manifest.integrity = PluginIntegrity {
            algorithm: "sha256-directory-v1".into(),
            digest,
            origin: origin.into(),
        };
        validate_manifest(root, &manifest)?;
        return Ok(LoadedPlugin {
            manifest,
            root: root.to_path_buf(),
            instruction_body: None,
        });
    }

    let skill = load_skill(root, origin, expected_digest)?;
    let network_hosts = match skill.manifest.permissions.network.trim() {
        "" | "none" | "deny" => BTreeSet::new(),
        value => BTreeSet::from([value.to_owned()]),
    };
    let manifest = ManagedPluginManifest {
        schema_version: PLUGIN_MANIFEST_SCHEMA_VERSION,
        id: skill.manifest.name.clone(),
        version: skill.manifest.version.clone(),
        kind: PluginKind::InstructionOnly,
        description: skill.manifest.description.clone(),
        instructions: vec!["SKILL.md".into()],
        required_capabilities: skill.manifest.tools.into_iter().collect(),
        tools: Vec::new(),
        scripts: Vec::new(),
        resources: skill.manifest.tests,
        mcp_servers: Vec::new(),
        authentication: PluginAuthentication::default(),
        permissions: PluginPermissions {
            write_paths: skill.manifest.permissions.write_paths.into_iter().collect(),
            network_hosts,
            ..PluginPermissions::default()
        },
        compatibility: skill.manifest.compatibility.medusa,
        integrity: PluginIntegrity {
            algorithm: "sha256-directory-v1".into(),
            digest: skill.digest,
            origin: skill.origin,
        },
    };
    validate_manifest(root, &manifest)?;
    Ok(LoadedPlugin {
        manifest,
        root: skill.root,
        instruction_body: Some(skill.body),
    })
}

pub fn validate_manifest(root: &Path, manifest: &ManagedPluginManifest) -> MedusaResult<()> {
    if manifest.schema_version != PLUGIN_MANIFEST_SCHEMA_VERSION {
        return Err(invalid("unsupported managed plugin schema version"));
    }
    if manifest.id.trim().is_empty()
        || manifest.version.trim().is_empty()
        || manifest.description.trim().is_empty()
        || manifest.compatibility.trim().is_empty()
        || manifest.integrity.algorithm != "sha256-directory-v1"
        || !valid_directory_digest(&manifest.integrity.digest)
        || manifest.integrity.origin.trim().is_empty()
    {
        return Err(invalid("managed plugin manifest is incomplete"));
    }
    if !manifest.id.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
    }) {
        return Err(invalid("managed plugin id must be lowercase kebab-case"));
    }
    if manifest.kind == PluginKind::InstructionOnly
        && (!manifest.tools.is_empty()
            || !manifest.scripts.is_empty()
            || !manifest.mcp_servers.is_empty()
            || manifest.permissions.process_spawn)
    {
        return Err(invalid(
            "instruction-only plugins cannot register executable tools, scripts, MCP servers, or process authority",
        ));
    }
    if manifest.kind != PluginKind::InstructionOnly
        && manifest.tools.is_empty()
        && manifest.scripts.is_empty()
        && manifest.mcp_servers.is_empty()
    {
        return Err(invalid(
            "executable plugin declares no executable component",
        ));
    }
    for relative in manifest
        .instructions
        .iter()
        .chain(&manifest.scripts)
        .chain(&manifest.resources)
    {
        validate_relative(root, relative)?;
    }
    if manifest.authentication.required && manifest.authentication.methods.is_empty() {
        return Err(invalid(
            "plugin authentication is required but no method is declared",
        ));
    }
    Ok(())
}

fn valid_directory_digest(digest: &str) -> bool {
    digest
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn validate_relative(root: &Path, relative: &str) -> MedusaResult<()> {
    let path = Path::new(relative);
    if relative.trim().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
        || !root.join(path).is_file()
    {
        return Err(invalid(format!(
            "plugin resource is missing or escapes its root: {relative}"
        )));
    }
    Ok(())
}

fn checksum_error(expected: &str, actual: &str) -> MedusaError {
    MedusaError::new(
        ErrorCode::ChecksumMismatch,
        ErrorCategory::Validation,
        format!("plugin digest mismatch: expected {expected}, got {actual}"),
    )
}

fn json_error(error: serde_json::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        error.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_md_is_instruction_only_and_cannot_create_tool_authority() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(
            directory.path().join("SKILL.md"),
            "---\nname: review\nversion: 1.0.0\ndescription: Review code.\ntools: [fs_read]\npermissions:\n  network: deny\n  write_paths: []\ncompatibility:\n  medusa: '>=1.0.0'\n---\nInspect evidence.\n",
        )
        .expect("skill");
        let digest = directory_digest(directory.path()).expect("digest");
        let plugin = load_managed_plugin(directory.path(), "fixture", &digest).expect("plugin");
        assert_eq!(plugin.manifest.kind, PluginKind::InstructionOnly);
        assert!(plugin.manifest.tools.is_empty());
        assert!(plugin.manifest.required_capabilities.contains("fs_read"));
        assert!(plugin.instruction_body.is_some());
    }

    #[test]
    fn instruction_only_manifest_rejects_executable_authority() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(directory.path().join("instructions.md"), "Use evidence.").expect("instructions");
        let manifest = ManagedPluginManifest {
            schema_version: PLUGIN_MANIFEST_SCHEMA_VERSION,
            id: "unsafe-plugin".into(),
            version: "1.0.0".into(),
            kind: PluginKind::InstructionOnly,
            description: "fixture".into(),
            instructions: vec!["instructions.md".into()],
            required_capabilities: BTreeSet::new(),
            tools: vec!["invented_tool".into()],
            scripts: Vec::new(),
            resources: Vec::new(),
            mcp_servers: Vec::new(),
            authentication: PluginAuthentication::default(),
            permissions: PluginPermissions::default(),
            compatibility: ">=1.0.0".into(),
            integrity: PluginIntegrity {
                algorithm: "sha256-directory-v1".into(),
                digest: format!("sha256:{}", "a".repeat(64)),
                origin: "fixture".into(),
            },
        };
        assert!(validate_manifest(directory.path(), &manifest).is_err());
    }
}
