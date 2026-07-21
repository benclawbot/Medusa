use std::path::Path;

use medusa_runtime::{
    skill_dependencies::{inspect_project_skill, validate_project_graph},
    skill_dependency_locks::{
        compute_dependency_lock, verify_dependency_lock, write_dependency_lock,
    },
};

pub(super) fn try_run(root: &Path, args: &[String]) -> Option<Result<(), String>> {
    match args.first().map(String::as_str) {
        Some("dependencies") => Some(inspect(root, &args[1..])),
        Some("validate-dependencies") => Some(validate(root, &args[1..])),
        Some("lock-dependencies") => Some(lock(root, &args[1..])),
        Some("verify-dependency-lock") => Some(verify_lock(root, &args[1..])),
        _ => None,
    }
}

pub(super) fn usage_lines() -> &'static str {
    "  medusa [--repo PATH] skills dependencies NAME [--json]\n  medusa [--repo PATH] skills validate-dependencies [--json]\n  medusa [--repo PATH] skills lock-dependencies NAME [--check] [--json]\n  medusa [--repo PATH] skills verify-dependency-lock NAME [--json]"
}

fn inspect(root: &Path, args: &[String]) -> Result<(), String> {
    let (name, json) = name_and_json(args)?;
    let report = inspect_project_skill(&root.join(".medusa/skills"), name)?;
    if json {
        print_json(&report)
    } else {
        println!("skill: {}", report.skill);
        println!("direct: {}", display(&report.direct));
        println!("load order: {}", display(&report.transitive_order));
        println!("dependents: {}", display(&report.reverse_dependents));
        Ok(())
    }
}

fn validate(root: &Path, args: &[String]) -> Result<(), String> {
    let json = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => return Err(usage()),
    };
    let skills = validate_project_graph(&root.join(".medusa/skills"))?;
    if json {
        print_json(&serde_json::json!({"valid": true, "skills": skills}))
    } else {
        println!("Validated {} approved project skill(s).", skills.len());
        Ok(())
    }
}

fn lock(root: &Path, args: &[String]) -> Result<(), String> {
    let (name, check, json) = lock_arguments(args)?;
    let skill_root = root.join(".medusa/skills");
    let receipt = if check {
        let current = compute_dependency_lock(&skill_root, name)?;
        let verified = verify_dependency_lock(&skill_root, name)?;
        if current.graph_sha256 != verified.graph_sha256 {
            return Err(format!("dependency lock for `{name}` is stale"));
        }
        current
    } else {
        write_dependency_lock(&skill_root, name)?
    };
    if json {
        print_json(&receipt)
    } else if check {
        println!("Dependency lock for `{name}` is current ({}).", receipt.graph_sha256);
        Ok(())
    } else {
        println!("Locked dependency graph for `{name}` ({}).", receipt.graph_sha256);
        Ok(())
    }
}

fn verify_lock(root: &Path, args: &[String]) -> Result<(), String> {
    let (name, json) = name_and_json(args)?;
    let report = verify_dependency_lock(&root.join(".medusa/skills"), name)?;
    if json {
        print_json(&report)
    } else {
        println!("Dependency lock for `{name}` is valid ({}).", report.graph_sha256);
        Ok(())
    }
}

fn name_and_json(args: &[String]) -> Result<(&str, bool), String> {
    match args {
        [name] => Ok((name, false)),
        [name, flag] if flag == "--json" => Ok((name, true)),
        _ => Err(usage()),
    }
}

fn lock_arguments(args: &[String]) -> Result<(&str, bool, bool), String> {
    let Some(name) = args.first() else {
        return Err(usage());
    };
    let mut check = false;
    let mut json = false;
    for flag in &args[1..] {
        match flag.as_str() {
            "--check" if !check => check = true,
            "--json" if !json => json = true,
            _ => return Err(usage()),
        }
    }
    Ok((name, check, json))
}

fn print_json(value: &impl serde::Serialize) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn display(values: &[String]) -> String {
    if values.is_empty() {
        "-".to_owned()
    } else {
        values.join(", ")
    }
}

fn usage() -> String {
    format!("Usage:\n{}", usage_lines())
}
