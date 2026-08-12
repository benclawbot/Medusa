//! Versioned, language-neutral executable skill packages.
//!
//! A package is untrusted repository content until this module validates it and a caller records
//! the resulting digest. The contract intentionally admits only read-only or bounded artifact
//! side effects; repository mutation, ambient secrets, and undeclared network access are rejected.

use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub const SKILL_MANIFEST_SCHEMA_VERSION: u16 = 1;
pub const SKILL_VALIDATION_SCHEMA_VERSION: u16 = 1;
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_SKILL_INSTRUCTIONS_BYTES: usize = 64 * 1024;
const MAX_ENTRYPOINTS: usize = 8;
const MAX_ARGUMENTS: usize = 32;
const MAX_DECLARED_ENV: usize = 16;
const MAX_ARTIFACTS: usize = 16;
const MAX_PACKAGE_FILES: usize = 128;
const MAX_PACKAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_OUTPUT_BYTES: u64 = 512 * 1024;
const MAX_RUNTIME_SECONDS: u64 = 300;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillRuntime {
    NativeCommand,
    Python,
    Node,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryAccess {
    None,
    ReadOnly,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicy {
    Denied,
    Brokered,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SideEffectClass {
    ReadOnly,
    Artifact,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkillResourceBudget {
    pub timeout_seconds: u64,
    pub cpu_time_seconds: u64,
    pub max_output_bytes: u64,
    pub max_processes: u16,
    pub max_memory_bytes: u64,
    pub max_disk_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutableSkill {
    pub name: String,
    pub runtime: SkillRuntime,
    /// Relative executable or interpreter path. It is resolved beneath the package root.
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub input_schema: serde_json::Value,
    pub output_schema: serde_json::Value,
    #[serde(default)]
    pub capabilities: BTreeSet<String>,
    /// Names are declarations only; the runner never forwards ambient secrets.
    #[serde(default)]
    pub env: BTreeSet<String>,
    pub repository_access: RepositoryAccess,
    pub network: NetworkPolicy,
    pub resources: SkillResourceBudget,
    pub side_effect: SideEffectClass,
    pub idempotent: bool,
    pub cancellation_supported: bool,
    #[serde(default)]
    pub tests: Vec<String>,
    #[serde(default)]
    pub verification: Vec<String>,
    #[serde(default)]
    pub artifacts: Vec<String>,
    /// Optional argument inserted before the bounded input file path.
    #[serde(default)]
    pub input_file_arg: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkillPackageManifest {
    pub schema_version: u16,
    pub id: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub scope: String,
    pub entrypoints: Vec<ExecutableSkill>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkillValidationReceipt {
    pub schema_version: u16,
    pub skill_id: String,
    pub skill_version: String,
    pub package_digest: String,
    pub validated_at: String,
    pub entrypoints: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedSkillPackage {
    pub root: PathBuf,
    pub manifest: SkillPackageManifest,
    pub receipt: SkillValidationReceipt,
}

impl SkillPackageManifest {
    pub fn entrypoint(&self, name: &str) -> MedusaResult<&ExecutableSkill> {
        self.entrypoints
            .iter()
            .find(|entrypoint| entrypoint.name == name)
            .ok_or_else(|| invalid(format!("skill entrypoint not found: {name}")))
    }
}

impl SkillValidationReceipt {
    pub fn matches(&self, package: &ValidatedSkillPackage) -> bool {
        self.schema_version == SKILL_VALIDATION_SCHEMA_VERSION
            && self.skill_id == package.manifest.id
            && self.skill_version == package.manifest.version
            && self.package_digest == package.receipt.package_digest
    }
}

pub fn validate_package(root: &Path) -> MedusaResult<ValidatedSkillPackage> {
    if !root.is_dir() {
        return Err(invalid(format!(
            "skill package is not a directory: {}",
            root.display()
        )));
    }
    let instructions = root.join("SKILL.md");
    let manifest_path = root.join("skill.json");
    let instructions_bytes = read_bounded(&instructions, MAX_SKILL_INSTRUCTIONS_BYTES)?;
    if instructions_bytes.is_empty() {
        return Err(invalid("skill package SKILL.md must not be empty"));
    }
    let manifest_bytes = read_bounded(&manifest_path, MAX_MANIFEST_BYTES)?;
    let manifest: SkillPackageManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| invalid(format!("invalid skill.json: {error}")))?;
    validate_manifest(root, &manifest)?;
    let package_digest = package_digest(root, &manifest, &instructions_bytes)?;
    let validated_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| internal(error.to_string()))?;
    let receipt = SkillValidationReceipt {
        schema_version: SKILL_VALIDATION_SCHEMA_VERSION,
        skill_id: manifest.id.clone(),
        skill_version: manifest.version.clone(),
        package_digest,
        validated_at,
        entrypoints: manifest
            .entrypoints
            .iter()
            .map(|entrypoint| entrypoint.name.clone())
            .collect(),
    };
    Ok(ValidatedSkillPackage {
        root: root.to_path_buf(),
        manifest,
        receipt,
    })
}

pub fn validate_input(input: &serde_json::Value) -> MedusaResult<()> {
    let bytes = serde_json::to_vec(input).map_err(|error| invalid(error.to_string()))?;
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(invalid(format!(
            "skill input exceeds {MAX_INPUT_BYTES} bytes"
        )));
    }
    if !input.is_object() {
        return Err(invalid("skill input must be a JSON object"));
    }
    Ok(())
}

pub fn copy_package(root: &Path, destination: &Path) -> MedusaResult<()> {
    let mut budget = CopyBudget::default();
    copy_tree(root, destination, &mut budget)
}

fn validate_manifest(root: &Path, manifest: &SkillPackageManifest) -> MedusaResult<()> {
    if manifest.schema_version != SKILL_MANIFEST_SCHEMA_VERSION {
        return Err(invalid(format!(
            "unsupported skill manifest schema {}",
            manifest.schema_version
        )));
    }
    validate_identifier(&manifest.id, "skill id")?;
    validate_version(&manifest.version)?;
    if manifest.description.trim().is_empty() || manifest.description.len() > 512 {
        return Err(invalid("skill description must be bounded and non-empty"));
    }
    if !matches!(manifest.scope.as_str(), "session" | "project" | "user" | "") {
        return Err(invalid(
            "skill scope must be session, project, user, or empty",
        ));
    }
    if manifest.entrypoints.is_empty() || manifest.entrypoints.len() > MAX_ENTRYPOINTS {
        return Err(invalid(format!(
            "skill must declare 1..={MAX_ENTRYPOINTS} entrypoints"
        )));
    }
    let mut names = BTreeSet::new();
    for entrypoint in &manifest.entrypoints {
        validate_identifier(&entrypoint.name, "entrypoint name")?;
        if !names.insert(&entrypoint.name) {
            return Err(invalid("skill entrypoint names must be unique"));
        }
        let program = safe_relative_path(&entrypoint.program, "entrypoint program")?;
        require_file(&root.join(program), "entrypoint program")?;
        if entrypoint.args.len() > MAX_ARGUMENTS
            || entrypoint.args.iter().any(|arg| arg.contains('\0'))
        {
            return Err(invalid(
                "entrypoint arguments are invalid or exceed the bound",
            ));
        }
        if !entrypoint.input_schema.is_object() || !entrypoint.output_schema.is_object() {
            return Err(invalid(
                "entrypoint input_schema and output_schema must be objects",
            ));
        }
        if entrypoint.capabilities.iter().any(|capability| {
            !matches!(
                capability.as_str(),
                "filesystem_read" | "artifact_write" | "network_broker"
            )
        }) {
            return Err(invalid("skill declares an unsupported capability"));
        }
        if entrypoint.env.len() > MAX_DECLARED_ENV
            || entrypoint.env.iter().any(|name| !valid_env_name(name))
        {
            return Err(invalid(
                "skill environment declarations are invalid or exceed the bound",
            ));
        }
        if matches!(entrypoint.network, NetworkPolicy::Brokered)
            && !entrypoint.capabilities.contains("network_broker")
        {
            return Err(invalid(
                "brokered network requires network_broker capability",
            ));
        }
        if matches!(entrypoint.side_effect, SideEffectClass::Artifact)
            && !entrypoint.capabilities.contains("artifact_write")
        {
            return Err(invalid(
                "artifact side effects require artifact_write capability",
            ));
        }
        if entrypoint.resources.timeout_seconds == 0
            || entrypoint.resources.timeout_seconds > MAX_RUNTIME_SECONDS
            || entrypoint.resources.cpu_time_seconds == 0
            || entrypoint.resources.cpu_time_seconds > entrypoint.resources.timeout_seconds
            || entrypoint.resources.max_output_bytes == 0
            || entrypoint.resources.max_output_bytes > MAX_OUTPUT_BYTES
            || entrypoint.resources.max_processes == 0
            || entrypoint.resources.max_memory_bytes == 0
            || entrypoint.resources.max_disk_bytes == 0
        {
            return Err(invalid(
                "skill resource budgets are missing or exceed policy bounds",
            ));
        }
        if !entrypoint.cancellation_supported {
            return Err(invalid("executable skills must support cancellation"));
        }
        for test in &entrypoint.tests {
            let test = safe_relative_path(test, "skill test")?;
            require_file(&root.join(test), "skill test")?;
        }
        if entrypoint.verification.len() > MAX_ARTIFACTS {
            return Err(invalid("skill verification entries exceed the bound"));
        }
        for verification in &entrypoint.verification {
            let verification = safe_relative_path(verification, "skill verification")?;
            require_file(&root.join(verification), "skill verification")?;
        }
        if entrypoint.artifacts.len() > MAX_ARTIFACTS {
            return Err(invalid("skill artifact entries exceed the bound"));
        }
        for artifact in &entrypoint.artifacts {
            safe_relative_path(artifact, "skill artifact")?;
        }
        if matches!(entrypoint.side_effect, SideEffectClass::Artifact)
            && entrypoint.artifacts.is_empty()
        {
            return Err(invalid(
                "artifact side effects require declared artifact paths",
            ));
        }
        if let Some(argument) = &entrypoint.input_file_arg {
            if argument.trim().is_empty() || argument.contains(['\0', ' ', '\t']) {
                return Err(invalid("input_file_arg must be one bounded argument"));
            }
        }
    }
    Ok(())
}

fn package_digest(
    root: &Path,
    manifest: &SkillPackageManifest,
    instructions: &[u8],
) -> MedusaResult<String> {
    let mut files = vec![PathBuf::from("SKILL.md"), PathBuf::from("skill.json")];
    for entrypoint in &manifest.entrypoints {
        files.push(PathBuf::from(&entrypoint.program));
        files.extend(entrypoint.tests.iter().map(PathBuf::from));
        files.extend(entrypoint.verification.iter().map(PathBuf::from));
    }
    files.sort();
    files.dedup();
    let mut digest = Sha256::new();
    for relative in files {
        let bytes = if relative == Path::new("SKILL.md") {
            instructions.to_vec()
        } else {
            read_bounded(&root.join(&relative), MAX_MANIFEST_BYTES)?
        };
        digest.update(relative.to_string_lossy().as_bytes());
        digest.update([0]);
        digest.update(bytes);
        digest.update([0]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[derive(Default)]
struct CopyBudget {
    files: usize,
    bytes: usize,
}

fn copy_tree(source: &Path, destination: &Path, budget: &mut CopyBudget) -> MedusaResult<()> {
    fs::create_dir_all(destination)?;
    let mut entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_path)?;
        if metadata.file_type().is_symlink() {
            return Err(invalid("skill package contains a symlink"));
        }
        if metadata.is_dir() {
            copy_tree(&source_path, &destination_path, budget)?;
        } else if metadata.is_file() {
            budget.files = budget.files.saturating_add(1);
            budget.bytes = budget.bytes.saturating_add(
                usize::try_from(metadata.len())
                    .map_err(|_| invalid("skill package file is too large"))?,
            );
            if budget.files > MAX_PACKAGE_FILES || budget.bytes > MAX_PACKAGE_BYTES {
                return Err(invalid("skill package exceeds copy bounds"));
            }
            fs::copy(source_path, destination_path)?;
        }
    }
    Ok(())
}

fn safe_relative_path<'a>(value: &'a str, field: &str) -> MedusaResult<&'a Path> {
    let path = Path::new(value);
    if value.trim().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(invalid(format!(
            "{field} must be a non-empty relative path"
        )));
    }
    Ok(path)
}

fn require_file(path: &Path, field: &str) -> MedusaResult<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| invalid(format!("{field} is unavailable: {error}")))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(invalid(format!("{field} must be a regular file")));
    }
    Ok(())
}

fn read_bounded(path: &Path, max: usize) -> MedusaResult<Vec<u8>> {
    let bytes =
        fs::read(path).map_err(|error| invalid(format!("read {}: {error}", path.display())))?;
    if bytes.len() > max {
        return Err(invalid(format!("{} exceeds {max} bytes", path.display())));
    }
    Ok(bytes)
}

fn validate_identifier(value: &str, field: &str) -> MedusaResult<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(invalid(format!(
            "{field} must be a bounded ASCII identifier"
        )));
    }
    Ok(())
}

fn valid_env_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn validate_version(value: &str) -> MedusaResult<()> {
    let pieces = value.split('.').collect::<Vec<_>>();
    if pieces.len() != 3
        || pieces
            .iter()
            .any(|piece| piece.is_empty() || piece.parse::<u64>().is_err())
    {
        return Err(invalid("skill version must use numeric MAJOR.MINOR.PATCH"));
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

fn internal(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        message,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use tempfile::tempdir;

    fn package() -> tempfile::TempDir {
        let root = tempdir().expect("tempdir");
        fs::create_dir_all(root.path().join("scripts")).expect("scripts");
        fs::create_dir_all(root.path().join("tests")).expect("tests");
        fs::write(root.path().join("SKILL.md"), "# Example\n").expect("skill");
        fs::write(root.path().join("scripts/run"), "fixture").expect("program");
        fs::write(root.path().join("tests/smoke"), "fixture").expect("test");
        let manifest = serde_json::json!({
            "schema_version": 1,
            "id": "example",
            "version": "1.0.0",
            "description": "Example executable skill",
            "scope": "project",
            "entrypoints": [{
                "name": "run",
                "runtime": "native_command",
                "program": "scripts/run",
                "args": [],
                "input_schema": {"type":"object"},
                "output_schema": {"type":"object"},
                "capabilities": ["filesystem_read"],
                "repository_access": "read_only",
                "network": "denied",
                "resources": {"timeout_seconds": 10,"cpu_time_seconds": 5,"max_output_bytes": 1024,"max_processes": 1,"max_memory_bytes": 1024,"max_disk_bytes": 1024},
                "side_effect": "read_only",
                "idempotent": true,
                "cancellation_supported": true,
                "tests": ["tests/smoke"]
            }]
        });
        fs::write(
            root.path().join("skill.json"),
            serde_json::to_vec_pretty(&manifest).expect("json"),
        )
        .expect("manifest");
        root
    }

    #[test]
    fn valid_package_produces_stable_shape_and_digest() {
        let root = package();
        let validated = validate_package(root.path()).expect("validate");
        assert_eq!(
            validated
                .manifest
                .entrypoint("run")
                .expect("entrypoint")
                .runtime,
            SkillRuntime::NativeCommand
        );
        assert_eq!(validated.receipt.package_digest.len(), 64);
        assert!(validated.receipt.entrypoints.contains(&"run".to_owned()));
    }

    #[test]
    fn undeclared_network_and_mutation_are_rejected() {
        let root = package();
        let mut manifest: SkillPackageManifest =
            serde_json::from_slice(&fs::read(root.path().join("skill.json")).expect("read"))
                .expect("parse");
        manifest.entrypoints[0].network = NetworkPolicy::Brokered;
        fs::write(
            root.path().join("skill.json"),
            serde_json::to_vec(&manifest).expect("write"),
        )
        .expect("manifest");
        assert!(validate_package(root.path()).is_err());
        manifest.entrypoints[0].network = NetworkPolicy::Denied;
        manifest.entrypoints[0].capabilities = BTreeSet::new();
        manifest.entrypoints[0].side_effect = SideEffectClass::Artifact;
        fs::write(
            root.path().join("skill.json"),
            serde_json::to_vec(&manifest).expect("write"),
        )
        .expect("manifest");
        assert!(validate_package(root.path()).is_err());
    }

    #[test]
    fn input_is_bounded_and_object_typed() {
        assert!(validate_input(&serde_json::json!({"value": 1})).is_ok());
        assert!(validate_input(&serde_json::json!([1, 2])).is_err());
        assert!(
            validate_input(&serde_json::json!({"value": "x".repeat(MAX_INPUT_BYTES)})).is_err()
        );
    }
}
