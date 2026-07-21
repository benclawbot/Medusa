use std::path::Path;

use medusa_runtime::skill_dependencies::{inspect_project_skill, validate_project_graph};

pub(super) fn try_run(root: &Path, args: &[String]) -> Option<Result<(), String>> {
    match args.first().map(String::as_str) {
        Some("dependencies") => Some(inspect(root, &args[1..])),
        Some("validate-dependencies") => Some(validate(root, &args[1..])),
        _ => None,
    }
}

pub(super) fn usage_lines() -> &'static str {
    "  medusa [--repo PATH] skills dependencies NAME [--json]\n  medusa [--repo PATH] skills validate-dependencies [--json]"
}

fn inspect(root: &Path, args: &[String]) -> Result<(), String> {
    let (name, json) = match args {
        [name] => (name.as_str(), false),
        [name, flag] if flag == "--json" => (name.as_str(), true),
        _ => return Err(usage()),
    };
    let report = inspect_project_skill(&root.join(".medusa/skills"), name)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
        );
    } else {
        println!("skill: {}", report.skill);
        println!("direct: {}", display(&report.direct));
        println!("load order: {}", display(&report.transitive_order));
        println!("dependents: {}", display(&report.reverse_dependents));
    }
    Ok(())
}

fn validate(root: &Path, args: &[String]) -> Result<(), String> {
    let json = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => return Err(usage()),
    };
    let skills = validate_project_graph(&root.join(".medusa/skills"))?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"valid": true, "skills": skills}))
                .map_err(|error| error.to_string())?
        );
    } else {
        println!("Validated {} approved project skill(s).", skills.len());
    }
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
