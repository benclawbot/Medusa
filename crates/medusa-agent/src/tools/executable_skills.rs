use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_skill::{
    RepositoryAccess, SKILL_VALIDATION_SCHEMA_VERSION, SkillRuntime, SkillValidationReceipt,
    ValidatedSkillPackage, copy_package, validate_input, validate_package,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::shell;
use crate::output_envelope::OutputMode;

#[derive(Clone, Debug, Deserialize)]
struct ExecuteInput {
    name: String,
    entrypoint: String,
    #[serde(default)]
    input: Value,
}

#[derive(Clone, Debug, Serialize)]
struct SkillExecutionReceipt {
    schema_version: u16,
    skill_id: String,
    skill_version: String,
    package_digest: String,
    entrypoint: String,
    input_digest: String,
    output_digest: String,
    status: &'static str,
}

pub(crate) fn run(repo: &Path, input: &Value, cancellation: &AtomicBool) -> MedusaResult<String> {
    let request: ExecuteInput = serde_json::from_value(input.clone()).map_err(|error| {
        MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            format!("invalid executable skill request: {error}"),
        )
    })?;
    validate_name(&request.name)?;
    validate_input(&request.input)?;
    let package_root = repo.join(".medusa/skills").join(&request.name);
    let package = validated_package(&package_root)?;
    if package.manifest.id != request.name {
        return Err(policy_denied(
            "executable skill package id must match its installed directory",
        ));
    }
    let entrypoint = package.manifest.entrypoint(&request.entrypoint)?;
    if !matches!(
        entrypoint.repository_access,
        RepositoryAccess::None | RepositoryAccess::ReadOnly
    ) {
        return Err(policy_denied(
            "executable skills cannot receive repository mutation access",
        ));
    }
    if cancellation.load(std::sync::atomic::Ordering::Acquire) {
        return Err(cancelled());
    }

    let run_root = temporary_run_root(&package)?;
    let result = execute_in_isolated_copy(&run_root, &package, entrypoint, &request, cancellation);
    let _ = fs::remove_dir_all(&run_root);
    result
}

fn execute_in_isolated_copy(
    run_root: &Path,
    package: &ValidatedSkillPackage,
    entrypoint: &medusa_skill::ExecutableSkill,
    request: &ExecuteInput,
    cancellation: &AtomicBool,
) -> MedusaResult<String> {
    let mut args = entrypoint.args.clone();
    let input_digest = digest_json(&request.input)?;
    if let Some(input_arg) = &entrypoint.input_file_arg {
        let input_path = run_root.join(".medusa-input.json");
        fs::write(&input_path, serde_json::to_vec(&request.input)?)?;
        args.push(input_arg.clone());
        args.push(".medusa-input.json".to_owned());
    } else if request.input != Value::Object(serde_json::Map::new()) {
        return Err(invalid(
            "entrypoint must declare input_file_arg for non-empty input",
        ));
    }

    let (program, command_args) = match entrypoint.runtime {
        SkillRuntime::NativeCommand => (entrypoint.program.clone(), args),
        SkillRuntime::Python => {
            let mut command_args = vec![entrypoint.program.clone()];
            command_args.extend(args);
            ("python".to_owned(), command_args)
        }
        SkillRuntime::Node => {
            let mut command_args = vec![entrypoint.program.clone()];
            command_args.extend(args);
            ("node".to_owned(), command_args)
        }
    };
    let program = if matches!(entrypoint.runtime, SkillRuntime::NativeCommand)
        && !Path::new(&program).is_absolute()
    {
        if cfg!(windows) {
            format!(r".\{program}")
        } else {
            format!("./{program}")
        }
    } else {
        program
    };
    let output = shell::run_cancellable(
        run_root,
        &program,
        &command_args,
        OutputMode::Compact,
        cancellation,
    )?;
    let output = bound_output(output, entrypoint.resources.max_output_bytes)?;
    let output_digest = digest_bytes(output.as_bytes());
    let receipt = SkillExecutionReceipt {
        schema_version: 1,
        skill_id: package.manifest.id.clone(),
        skill_version: package.manifest.version.clone(),
        package_digest: package.receipt.package_digest.clone(),
        entrypoint: entrypoint.name.clone(),
        input_digest,
        output_digest,
        status: "completed",
    };
    Ok(format!(
        "{}\n[executable-skill provenance={}]",
        output,
        serde_json::to_string(&receipt).map_err(|error| invalid(error.to_string()))?
    ))
}

fn validated_package(root: &Path) -> MedusaResult<ValidatedSkillPackage> {
    let package = validate_package(root)?;
    let receipt_path = root.join("skill.validation.json");
    let receipt_bytes = fs::read(&receipt_path).map_err(|_| {
        policy_denied("executable skill has not passed explicit package validation")
    })?;
    let receipt: SkillValidationReceipt = serde_json::from_slice(&receipt_bytes)
        .map_err(|_| policy_denied("executable skill validation receipt is corrupt"))?;
    if receipt.schema_version != SKILL_VALIDATION_SCHEMA_VERSION
        || receipt.package_digest != package.receipt.package_digest
        || receipt.skill_id != package.manifest.id
        || receipt.skill_version != package.manifest.version
    {
        return Err(policy_denied(
            "executable skill package changed after validation; validate it again",
        ));
    }
    Ok(package)
}

fn temporary_run_root(package: &ValidatedSkillPackage) -> MedusaResult<PathBuf> {
    let root = std::env::temp_dir().join(format!(
        "medusa-skill-{}-{}",
        std::process::id(),
        &package.receipt.package_digest[..16]
    ));
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    copy_package(&package.root, &root)?;
    Ok(root)
}

fn digest_json(value: &Value) -> MedusaResult<String> {
    Ok(digest_bytes(&serde_json::to_vec(value)?))
}

fn digest_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn validate_name(name: &str) -> MedusaResult<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\'])
        || name.contains("..")
    {
        return Err(invalid("skill name must be one directory name"));
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

fn policy_denied(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::PolicyDenied, ErrorCategory::Policy, message)
}

fn cancelled() -> MedusaError {
    MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Execution,
        "executable skill cancelled",
    )
}

fn bound_output(output: String, maximum: u64) -> MedusaResult<String> {
    let maximum =
        usize::try_from(maximum).map_err(|_| invalid("skill output bound is too large"))?;
    if output.len() <= maximum {
        return Ok(output);
    }
    const MARKER: &str = "\n[executable-skill output truncated]";
    if maximum <= MARKER.len() {
        let mut bounded = output;
        bounded.truncate(maximum);
        while !bounded.is_char_boundary(bounded.len()) {
            bounded.pop();
        }
        return Ok(bounded);
    }
    let mut bounded = output;
    bounded.truncate(maximum - MARKER.len());
    while !bounded.is_char_boundary(bounded.len()) {
        bounded.pop();
    }
    bounded.push_str(MARKER);
    Ok(bounded)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn output_is_bounded_without_splitting_utf8() {
        let output = bound_output("alpha-λ".to_owned(), 8).expect("bounded output");
        assert!(output.len() <= 8);
        assert!(std::str::from_utf8(output.as_bytes()).is_ok());
    }

    #[test]
    fn tiny_output_bound_remains_hard() {
        let output = bound_output("long output".to_owned(), 3).expect("bounded output");
        assert!(output.len() <= 3);
    }

    #[test]
    fn execution_requires_an_explicit_validation_receipt() {
        let repo = tempdir().expect("repository");
        let package_root = repo.path().join(".medusa/skills/example/scripts");
        fs::create_dir_all(&package_root).expect("package directory");
        fs::write(
            repo.path().join(".medusa/skills/example/SKILL.md"),
            "# Example\n",
        )
        .expect("instructions");
        fs::write(package_root.join("run"), "placeholder\n").expect("program");
        fs::write(
            repo.path().join(".medusa/skills/example/skill.json"),
            serde_json::to_vec(&json!({
                "schema_version": 1,
                "id": "example",
                "version": "1.0.0",
                "description": "Example executable skill",
                "scope": "project",
                "entrypoints": [{
                    "name": "run",
                    "runtime": "native_command",
                    "program": "scripts/run",
                    "input_schema": {"type": "object"},
                    "output_schema": {"type": "object"},
                    "capabilities": ["filesystem_read"],
                    "repository_access": "read_only",
                    "network": "denied",
                    "resources": {"timeout_seconds": 10,"cpu_time_seconds": 5,"max_output_bytes": 1024,"max_processes": 1,"max_memory_bytes": 1024,"max_disk_bytes": 1024},
                    "side_effect": "read_only",
                    "idempotent": true,
                    "cancellation_supported": true
                }]
            }))
            .expect("manifest"),
        )
        .expect("manifest file");
        let cancellation = AtomicBool::new(false);
        let error = run(
            repo.path(),
            &json!({"name":"example","entrypoint":"run","input":{}}),
            &cancellation,
        )
        .expect_err("unvalidated package");
        assert_eq!(error.code, ErrorCode::PolicyDenied);
    }
}
