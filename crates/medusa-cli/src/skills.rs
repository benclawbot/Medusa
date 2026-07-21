use std::{
    env, fs,
    path::{Component, Path, PathBuf},
};

use serde::Serialize;
use serde_json::{Value, json};

const PROPOSAL_ROOT: &str = ".medusa/learning/skill-proposals";
const ACTIVE_ROOT: &str = ".medusa/skills";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ProposalSummary {
    name: String,
    status: String,
    revision: u64,
    confidence_milli: u64,
    requires_approval: bool,
    source_lessons: usize,
}

pub(super) fn run(args: &[String]) -> Result<(), String> {
    let (repo, command_args) = split_global_repo(args)?;
    let root = match repo {
        Some(path) => path,
        None => env::current_dir().map_err(|error| format!("resolve current directory: {error}"))?,
    };
    let Some(command) = command_args.first().map(String::as_str) else {
        return Err(usage());
    };
    match command {
        "list" => list(&root, &command_args[1..]),
        "show" => show(&root, &command_args[1..]),
        "approve" => approve(&root, &command_args[1..]),
        "reject" => reject(&root, &command_args[1..]),
        "help" | "--help" | "-h" => {
            println!("{}", usage());
            Ok(())
        }
        other => Err(format!("unknown skills command `{other}`\n{}", usage())),
    }
}

fn usage() -> String {
    "Usage:\n  medusa [--repo PATH] skills list [--json]\n  medusa [--repo PATH] skills show NAME [--json]\n  medusa [--repo PATH] skills approve NAME\n  medusa [--repo PATH] skills reject NAME [--reason TEXT]".to_owned()
}

fn split_global_repo(args: &[String]) -> Result<(Option<PathBuf>, Vec<String>), String> {
    let mut repo = None;
    let mut command = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let value = &args[index];
        if value == "--repo" {
            let Some(path) = args.get(index + 1) else {
                return Err("--repo requires a path".to_owned());
            };
            repo = Some(PathBuf::from(path));
            index += 2;
        } else if let Some(path) = value.strip_prefix("--repo=") {
            if path.is_empty() {
                return Err("--repo requires a path".to_owned());
            }
            repo = Some(PathBuf::from(path));
            index += 1;
        } else {
            command.push(value.clone());
            index += 1;
        }
    }
    Ok((repo, command))
}

fn list(root: &Path, args: &[String]) -> Result<(), String> {
    let json_output = parse_json_flag(args)?;
    let proposal_root = root.join(PROPOSAL_ROOT);
    let mut proposals = Vec::new();
    if proposal_root.is_dir() {
        for entry in fs::read_dir(&proposal_root)
            .map_err(|error| format!("read {}: {error}", proposal_root.display()))?
        {
            let entry = entry.map_err(|error| format!("read proposal entry: {error}"))?;
            if !entry.path().is_dir() {
                continue;
            }
            let manifest = read_manifest(&entry.path())?;
            proposals.push(summary(&manifest)?);
        }
    }
    proposals.sort_by(|left, right| left.name.cmp(&right.name));
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&proposals)
                .map_err(|error| format!("serialize proposals: {error}"))?
        );
    } else if proposals.is_empty() {
        println!("No skill proposals found.");
    } else {
        for proposal in proposals {
            println!(
                "{}\t{}\trevision={}\tconfidence={}\tapproval={}\tlessons={}",
                proposal.name,
                proposal.status,
                proposal.revision,
                proposal.confidence_milli,
                proposal.requires_approval,
                proposal.source_lessons
            );
        }
    }
    Ok(())
}

fn show(root: &Path, args: &[String]) -> Result<(), String> {
    let (name, json_output) = parse_named_json(args)?;
    let directory = proposal_directory(root, name)?;
    let manifest = read_manifest(&directory)?;
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&manifest)
                .map_err(|error| format!("serialize manifest: {error}"))?
        );
    } else {
        let skill = fs::read_to_string(directory.join("SKILL.md"))
            .map_err(|error| format!("read proposal skill: {error}"))?;
        println!("{skill}");
        println!(
            "\n--- Manifest ---\n{}",
            serde_json::to_string_pretty(&manifest)
                .map_err(|error| format!("serialize manifest: {error}"))?
        );
    }
    Ok(())
}

fn approve(root: &Path, args: &[String]) -> Result<(), String> {
    if args.len() != 1 {
        return Err(usage());
    }
    let name = &args[0];
    let directory = proposal_directory(root, name)?;
    let mut manifest = read_manifest(&directory)?;
    require_proposed(&manifest)?;
    validate_manifest_name(&manifest, name)?;

    let expected = format!("{ACTIVE_ROOT}/{name}/SKILL.md");
    let install_path = manifest
        .get("proposed_install_path")
        .and_then(Value::as_str)
        .ok_or_else(|| "manifest is missing proposed_install_path".to_owned())?;
    if install_path != expected {
        return Err(format!(
            "refusing unexpected install path `{install_path}`; expected `{expected}`"
        ));
    }

    let source = directory.join("SKILL.md");
    if !source.is_file() {
        return Err(format!("proposal skill is missing: {}", source.display()));
    }
    let destination = root.join(ACTIVE_ROOT).join(name).join("SKILL.md");
    if destination.exists() {
        return Err(format!(
            "active skill already exists; refusing overwrite: {}",
            destination.display()
        ));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| "active skill destination has no parent".to_owned())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("create {}: {error}", parent.display()))?;
    let content = fs::read(&source).map_err(|error| format!("read {}: {error}", source.display()))?;
    atomic_write(&destination, &content)?;

    set_review(&mut manifest, "approved", None)?;
    write_manifest(&directory, &manifest)?;
    println!("Approved `{name}` and installed {}", destination.display());
    Ok(())
}

fn reject(root: &Path, args: &[String]) -> Result<(), String> {
    let Some(name) = args.first() else {
        return Err(usage());
    };
    let reason = parse_reason(&args[1..])?;
    let directory = proposal_directory(root, name)?;
    let mut manifest = read_manifest(&directory)?;
    require_proposed(&manifest)?;
    validate_manifest_name(&manifest, name)?;
    set_review(&mut manifest, "rejected", reason.as_deref())?;
    write_manifest(&directory, &manifest)?;
    println!("Rejected `{name}`.");
    Ok(())
}

fn parse_json_flag(args: &[String]) -> Result<bool, String> {
    match args {
        [] => Ok(false),
        [flag] if flag == "--json" => Ok(true),
        _ => Err(usage()),
    }
}

fn parse_named_json(args: &[String]) -> Result<(&str, bool), String> {
    match args {
        [name] => Ok((name, false)),
        [name, flag] if flag == "--json" => Ok((name, true)),
        _ => Err(usage()),
    }
}

fn parse_reason(args: &[String]) -> Result<Option<String>, String> {
    match args {
        [] => Ok(None),
        [flag, reason] if flag == "--reason" && !reason.trim().is_empty() => {
            Ok(Some(reason.trim().to_owned()))
        }
        [value] if value.starts_with("--reason=") => {
            let reason = value.trim_start_matches("--reason=").trim();
            if reason.is_empty() {
                Err("--reason must not be empty".to_owned())
            } else {
                Ok(Some(reason.to_owned()))
            }
        }
        _ => Err(usage()),
    }
}

fn proposal_directory(root: &Path, name: &str) -> Result<PathBuf, String> {
    validate_name(name)?;
    let directory = root.join(PROPOSAL_ROOT).join(name);
    if !directory.is_dir() {
        return Err(format!("skill proposal not found: `{name}`"));
    }
    Ok(directory)
}

fn validate_name(name: &str) -> Result<(), String> {
    let path = Path::new(name);
    if name.is_empty()
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
        || name == "."
        || name == ".."
    {
        return Err(format!("invalid skill proposal name `{name}`"));
    }
    Ok(())
}

fn read_manifest(directory: &Path) -> Result<Value, String> {
    let path = directory.join("manifest.json");
    let bytes = fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn write_manifest(directory: &Path, manifest: &Value) -> Result<(), String> {
    let content = serde_json::to_vec_pretty(manifest)
        .map_err(|error| format!("serialize manifest: {error}"))?;
    atomic_write(&directory.join("manifest.json"), &content)
}

fn atomic_write(path: &Path, content: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, content)
        .map_err(|error| format!("write {}: {error}", temporary.display()))?;
    fs::rename(&temporary, path).map_err(|error| {
        format!(
            "replace {} with {}: {error}",
            path.display(),
            temporary.display()
        )
    })
}

fn require_proposed(manifest: &Value) -> Result<(), String> {
    let status = manifest
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let requires_approval = manifest
        .get("requires_approval")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if status != "proposed" || !requires_approval {
        return Err(format!(
            "proposal is not awaiting review (status={status}, requires_approval={requires_approval})"
        ));
    }
    Ok(())
}

fn validate_manifest_name(manifest: &Value, expected: &str) -> Result<(), String> {
    let actual = manifest
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "manifest is missing name".to_owned())?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "manifest name `{actual}` does not match proposal directory `{expected}`"
        ))
    }
}

fn set_review(manifest: &mut Value, status: &str, reason: Option<&str>) -> Result<(), String> {
    let object = manifest
        .as_object_mut()
        .ok_or_else(|| "manifest root must be an object".to_owned())?;
    object.insert("status".to_owned(), Value::String(status.to_owned()));
    object.insert("requires_approval".to_owned(), Value::Bool(false));
    object.insert("review_decision".to_owned(), Value::String(status.to_owned()));
    if let Some(reason) = reason {
        object.insert("review_reason".to_owned(), Value::String(reason.to_owned()));
    } else {
        object.remove("review_reason");
    }
    Ok(())
}

fn summary(manifest: &Value) -> Result<ProposalSummary, String> {
    Ok(ProposalSummary {
        name: manifest
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "manifest is missing name".to_owned())?
            .to_owned(),
        status: manifest
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        revision: manifest.get("revision").and_then(Value::as_u64).unwrap_or(1),
        confidence_milli: manifest
            .get("confidence_milli")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        requires_approval: manifest
            .get("requires_approval")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        source_lessons: manifest
            .get("source_lesson_ids")
            .and_then(Value::as_array)
            .map_or(1, Vec::len),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal(root: &Path, name: &str) -> PathBuf {
        let directory = root.join(PROPOSAL_ROOT).join(name);
        fs::create_dir_all(&directory).expect("proposal directory");
        fs::write(directory.join("SKILL.md"), "# Verified skill\n").expect("skill");
        fs::write(
            directory.join("manifest.json"),
            serde_json::to_vec_pretty(&json!({
                "schema_version": 3,
                "name": name,
                "status": "proposed",
                "confidence_milli": 900,
                "revision": 2,
                "source_lesson_ids": ["one", "two"],
                "proposed_install_path": format!("{ACTIVE_ROOT}/{name}/SKILL.md"),
                "requires_approval": true
            }))
            .expect("manifest json"),
        )
        .expect("manifest");
        directory
    }

    #[test]
    fn approval_installs_without_overwriting_and_records_decision() {
        let temp = tempfile::tempdir().expect("tempdir");
        let directory = proposal(temp.path(), "verify-package");
        approve(temp.path(), &["verify-package".to_owned()]).expect("approve");
        assert!(
            temp.path()
                .join(ACTIVE_ROOT)
                .join("verify-package/SKILL.md")
                .is_file()
        );
        let manifest = read_manifest(&directory).expect("manifest");
        assert_eq!(manifest["status"], "approved");
        assert_eq!(manifest["requires_approval"], false);
        assert!(approve(temp.path(), &["verify-package".to_owned()]).is_err());
    }

    #[test]
    fn rejection_never_activates_skill() {
        let temp = tempfile::tempdir().expect("tempdir");
        let directory = proposal(temp.path(), "verify-package");
        reject(
            temp.path(),
            &[
                "verify-package".to_owned(),
                "--reason".to_owned(),
                "too broad".to_owned(),
            ],
        )
        .expect("reject");
        let manifest = read_manifest(&directory).expect("manifest");
        assert_eq!(manifest["status"], "rejected");
        assert_eq!(manifest["review_reason"], "too broad");
        assert!(!temp.path().join(ACTIVE_ROOT).exists());
    }

    #[test]
    fn approval_refuses_unexpected_install_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let directory = proposal(temp.path(), "verify-package");
        let mut manifest = read_manifest(&directory).expect("manifest");
        manifest["proposed_install_path"] = json!("../../outside/SKILL.md");
        write_manifest(&directory, &manifest).expect("write");
        assert!(approve(temp.path(), &["verify-package".to_owned()]).is_err());
        assert!(!temp.path().join(ACTIVE_ROOT).exists());
    }

    #[test]
    fn names_cannot_escape_proposal_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        assert!(proposal_directory(temp.path(), "../escape").is_err());
        assert!(proposal_directory(temp.path(), "nested/name").is_err());
    }
}
