use std::{
    env, fs,
    path::{Component, Path, PathBuf},
};

use medusa_core::storage;
use medusa_skill::validate_package;
use serde_json::json;

const ACTIVE_ROOT: &str = ".medusa/skills";

pub(super) fn run(args: &[String]) -> Result<(), String> {
    let (repo, command_args) = split_global_repo(args)?;
    let root = repo.map_or_else(
        || env::current_dir().map_err(|error| format!("resolve current directory: {error}")),
        Ok,
    )?;
    match command_args.as_slice() {
        [command, rest @ ..] => match command.as_str() {
            "validate" => validate(&root, rest),
            "scaffold" => scaffold(&root, rest),
            "help" | "--help" | "-h" => {
                println!("{}", usage());
                Ok(())
            }
            _ => Err(format!("unknown skills command {command}\n{}", usage())),
        },
        [] => Err(usage()),
    }
}

fn usage() -> String {
    "Usage:\n  medusa [--repo PATH] skills validate NAME [--json]\n  medusa [--repo PATH] skills scaffold NAME --program PATH [--runtime native_command|python|node]"
        .to_owned()
}

fn validate(root: &Path, args: &[String]) -> Result<(), String> {
    let (name, json_output) = parse_named_json(args)?;
    validate_name(name)?;
    let package_root = root.join(ACTIVE_ROOT).join(name);
    let package = validate_package(&package_root).map_err(|error| error.to_string())?;
    if package.manifest.id != name {
        return Err(format!(
            "skill manifest id {} does not match installed name {name}",
            package.manifest.id
        ));
    }
    let receipt_path = package_root.join("skill.validation.json");
    let receipt = serde_json::to_vec_pretty(&package.receipt)
        .map_err(|error| format!("serialize validation receipt: {error}"))?;
    atomic_write(&receipt_path, &receipt)?;
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&package.receipt)
                .map_err(|error| format!("serialize validation receipt: {error}"))?
        );
    } else {
        println!("Validated {name} ({})", package.receipt.package_digest);
    }
    Ok(())
}

fn scaffold(root: &Path, args: &[String]) -> Result<(), String> {
    let (name, program, runtime) = parse_scaffold_args(args)?;
    validate_name(name)?;
    let program_path = Path::new(&program);
    if program_path.is_absolute()
        || program_path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
        || program.trim().is_empty()
    {
        return Err("--program must be a non-empty package-relative path".to_owned());
    }
    let package_root = root.join(ACTIVE_ROOT).join(name);
    if package_root.exists() {
        return Err(format!(
            "refusing to overwrite existing package: {}",
            package_root.display()
        ));
    }
    if let Some(parent) = package_root.join(program_path).parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    fs::create_dir_all(package_root.join("tests"))
        .map_err(|error| format!("create tests directory: {error}"))?;
    atomic_write(
        &package_root.join("SKILL.md"),
        b"# Executable skill\n\nReplace the scaffold program and document its bounded JSON contract here.\n",
    )?;
    atomic_write(
        &package_root.join(&program),
        b"Replace this placeholder with the package entrypoint.\n",
    )?;
    atomic_write(&package_root.join("tests/smoke"), b"scaffold smoke test\n")?;
    let manifest = json!({
        "schema_version": 1,
        "id": name,
        "version": "0.1.0",
        "description": "Scaffolded executable skill",
        "scope": "project",
        "entrypoints": [{
            "name": "run",
            "runtime": runtime,
            "program": program,
            "args": [],
            "input_schema": {"type": "object"},
            "output_schema": {"type": "object"},
            "capabilities": ["filesystem_read"],
            "env": [],
            "repository_access": "read_only",
            "network": "denied",
            "resources": {"timeout_seconds": 60,"cpu_time_seconds": 30,"max_output_bytes": 131072,"max_processes": 1,"max_memory_bytes": 134217728,"max_disk_bytes": 16777216},
            "side_effect": "read_only",
            "idempotent": true,
            "cancellation_supported": true,
            "tests": ["tests/smoke"],
            "verification": ["tests/smoke"],
            "artifacts": [],
            "input_file_arg": "--input-file"
        }]
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("serialize scaffold manifest: {error}"))?;
    atomic_write(&package_root.join("skill.json"), &manifest_bytes)?;
    println!("Scaffolded executable skill at {}", package_root.display());
    Ok(())
}

fn parse_scaffold_args(args: &[String]) -> Result<(&str, String, &str), String> {
    let Some(name) = args.first().map(String::as_str) else {
        return Err(usage());
    };
    let mut program = None;
    let mut runtime = "native_command";
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--program" => {
                program = Some(
                    args.get(index + 1)
                        .ok_or_else(|| "--program requires a path".to_owned())?
                        .clone(),
                );
                index += 2;
            }
            value if value.starts_with("--program=") => {
                program = Some(value.trim_start_matches("--program=").to_owned());
                index += 1;
            }
            "--runtime" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--runtime requires a value".to_owned())?;
                runtime = valid_runtime(value)?;
                index += 2;
            }
            value if value.starts_with("--runtime=") => {
                runtime = valid_runtime(value.trim_start_matches("--runtime="))?;
                index += 1;
            }
            _ => return Err(usage()),
        }
    }
    Ok((
        name,
        program.ok_or_else(|| "--program requires a path".to_owned())?,
        runtime,
    ))
}

fn valid_runtime(value: &str) -> Result<&str, String> {
    matches!(value, "native_command" | "python" | "node")
        .then_some(value)
        .ok_or_else(|| "--runtime must be native_command, python, or node".to_owned())
}

fn split_global_repo(args: &[String]) -> Result<(Option<PathBuf>, Vec<String>), String> {
    let mut repo = None;
    let mut command = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--repo" => {
                repo = Some(PathBuf::from(
                    args.get(index + 1)
                        .ok_or_else(|| "--repo requires a path".to_owned())?,
                ));
                index += 2;
            }
            value if value.starts_with("--repo=") => {
                let path = value.trim_start_matches("--repo=");
                if path.is_empty() {
                    return Err("--repo requires a path".to_owned());
                }
                repo = Some(PathBuf::from(path));
                index += 1;
            }
            value => {
                command.push(value.to_owned());
                index += 1;
            }
        }
    }
    Ok((repo, command))
}

fn parse_named_json(args: &[String]) -> Result<(&str, bool), String> {
    match args {
        [name] => Ok((name, false)),
        [name, flag] if flag == "--json" => Ok((name, true)),
        _ => Err(usage()),
    }
}

fn validate_name(name: &str) -> Result<(), String> {
    let path = Path::new(name);
    if name.is_empty()
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
        || name == "."
        || name == ".."
    {
        return Err(format!("invalid skill name {name}"));
    }
    Ok(())
}

fn atomic_write(path: &Path, content: &[u8]) -> Result<(), String> {
    storage::atomic_write(path, content)
        .map_err(|error| format!("write {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_skill_scaffold_can_be_explicitly_validated() {
        let temp = tempfile::tempdir().expect("tempdir");
        scaffold(
            temp.path(),
            &[
                "example".to_owned(),
                "--program".to_owned(),
                "scripts/run".to_owned(),
            ],
        )
        .expect("scaffold");
        validate(temp.path(), &["example".to_owned()]).expect("validate");
        assert!(
            temp.path()
                .join(ACTIVE_ROOT)
                .join("example/skill.validation.json")
                .is_file()
        );
    }

    #[test]
    fn scaffold_rejects_package_escape_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        assert!(
            scaffold(
                temp.path(),
                &[
                    "example".to_owned(),
                    "--program".to_owned(),
                    "../run".to_owned(),
                ],
            )
            .is_err()
        );
    }
}
