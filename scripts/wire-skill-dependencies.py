from pathlib import Path


def replace(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    if old not in text:
        raise SystemExit(f"missing anchor in {path}: {old[:100]!r}")
    file.write_text(text.replace(old, new, 1))


# Export the dependency engine and route approved project skills through it.
replace(
    "crates/medusa-runtime/src/lib.rs",
    "mod error;\npub mod prompt;",
    "mod error;\npub mod prompt;\npub mod skill_dependencies;",
)
replace(
    "crates/medusa-runtime/src/lib.rs",
    "use support::{\n    SelectedSkill, UpdateState, configure_model, credential_environment, discover_skills,",
    "use support::{\n    SelectedSkill, UpdateState, configure_model, credential_environment, discover_skills,",
)
replace(
    "crates/medusa-runtime/src/lib.rs",
    "fn run_prompt(\n",
    '''fn load_skill(repo: &std::path::Path, selector: &str) -> Result<SelectedSkill, RuntimeError> {
    let selector = selector.trim();
    let (name, requested_scope) = selector
        .rsplit_once('@')
        .map_or((selector, None), |(name, scope)| (name, Some(scope)));
    let approved_root = repo.join(".medusa/skills");
    if requested_scope != Some("user") && approved_root.join(name).join("SKILL.md").is_file() {
        let resolved = skill_dependencies::resolve_project_skill(
            &approved_root,
            name,
            64_000,
        )
        .map_err(RuntimeError::InvalidCommand)?;
        return Ok(SelectedSkill {
            name: name.to_owned(),
            scope: "project".to_owned(),
            content: resolved.content,
        });
    }
    load_selected_skill(repo, selector)
}

fn run_prompt(
''',
)
replace(
    "crates/medusa-runtime/src/lib.rs",
    "let skill = load_selected_skill(&state.repo, &selector)?;",
    "let skill = load_skill(&state.repo, &selector)?;",
)

# Make reports serializable and callable by the CLI.
dep = Path("crates/medusa-runtime/src/skill_dependencies.rs")
text = dep.read_text()
text = text.replace("use serde_json::Value;", "use serde::Serialize;\nuse serde_json::Value;")
text = text.replace("#[derive(Clone, Debug, Eq, PartialEq)]\npub(crate) struct ResolvedSkillGraph", "#[derive(Clone, Debug, Eq, PartialEq, Serialize)]\npub struct ResolvedSkillGraph")
text = text.replace("#[derive(Clone, Debug, Eq, PartialEq)]\npub(crate) struct DependencyInspection", "#[derive(Clone, Debug, Eq, PartialEq, Serialize)]\npub struct DependencyInspection")
text = text.replace("pub(crate) ", "pub ")
append = r'''

pub fn validate_restorable_skill(active_root: &Path, candidate: &Path, name: &str) -> Result<(), String> {
    validate_name(name)?;
    let graph = load_graph(active_root)?;
    let manifest = candidate.join(MANIFEST);
    if !manifest.exists() {
        return Ok(());
    }
    let canonical_candidate = fs::canonicalize(candidate)
        .map_err(|error| format!("resolve {}: {error}", candidate.display()))?;
    let canonical_manifest = fs::canonicalize(&manifest)
        .map_err(|error| format!("resolve {}: {error}", manifest.display()))?;
    if !canonical_manifest.starts_with(&canonical_candidate) {
        return Err(format!("dependency manifest for `{name}` escapes quarantined skill directory"));
    }
    let value: Value = serde_json::from_slice(
        &fs::read(&canonical_manifest)
            .map_err(|error| format!("read {}: {error}", canonical_manifest.display()))?,
    )
    .map_err(|error| format!("parse {}: {error}", canonical_manifest.display()))?;
    if value.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(format!("{} requires schema_version 1", canonical_manifest.display()));
    }
    let requires = value
        .get("requires")
        .map_or(Ok(&[][..]), |value| value.as_array().map(Vec::as_slice).ok_or_else(|| format!("{}.requires must be an array", canonical_manifest.display())))?;
    let mut seen = BTreeSet::new();
    for value in requires {
        let dependency = value.as_str().ok_or_else(|| format!("{}.requires entries must be strings", canonical_manifest.display()))?;
        validate_name(dependency)?;
        if dependency == name {
            return Err(format!("skill `{name}` cannot depend on itself"));
        }
        if !seen.insert(dependency) {
            return Err(format!("skill `{name}` declares duplicate dependency `{dependency}`"));
        }
        if !graph.contains_key(dependency) {
            return Err(format!("skill `{name}` requires unavailable approved project skill `{dependency}`"));
        }
    }
    Ok(())
}
'''
if "pub fn validate_restorable_skill" not in text:
    text += append
dep.write_text(text)

replace(
    "crates/medusa-runtime/Cargo.toml",
    'png = "0.17"\n',
    'png = "0.17"\nserde.workspace = true\n',
)
replace(
    "crates/medusa-cli/Cargo.toml",
    'medusa-provider = { path = "../medusa-provider" }\n',
    'medusa-provider = { path = "../medusa-provider" }\nmedusa-runtime = { path = "../medusa-runtime" }\n',
)

# Add operator commands.
Path("crates/medusa-cli/src/skill_dependencies.rs").write_text(r'''use std::path::Path;

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
        println!("{}", serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?);
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
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({"valid": true, "skills": skills})).map_err(|error| error.to_string())?);
    } else {
        println!("Validated {} approved project skill(s).", skills.len());
    }
    Ok(())
}

fn display(values: &[String]) -> String {
    if values.is_empty() { "-".to_owned() } else { values.join(", ") }
}

fn usage() -> String { format!("Usage:\n{}", usage_lines()) }
''')

replace(
    "crates/medusa-cli/src/wrapper.rs",
    "mod skill_graduation;",
    "mod skill_dependencies;\nmod skill_graduation;",
)
old_router = '''        let graduation = skill_graduation::try_run(&repo, &command_args);
        let lifecycle = if graduation.is_none() {
            skill_lifecycle::try_run(&repo, &command_args)
        } else {
            None
        };
        let probation = if graduation.is_none() && lifecycle.is_none() {
            skill_probation::try_run(&repo, &command_args)
        } else {
            None
        };
        let usage = if graduation.is_some() {
            Some(skill_graduation::usage_line())
        } else if lifecycle.is_some() {
            Some(skill_lifecycle::usage_lines())
        } else if probation.is_some() {
            Some(skill_probation::usage_line())
        } else {
            None
        };
        let result = match (graduation, lifecycle, probation) {
            (Some(result), _, _) | (_, Some(result), _) | (_, _, Some(result)) => result,
            (None, None, None) => skills::run(&skill_args),
        };'''
new_router = '''        let dependencies = skill_dependencies::try_run(&repo, &command_args);
        let graduation = if dependencies.is_none() {
            skill_graduation::try_run(&repo, &command_args)
        } else {
            None
        };
        let lifecycle = if dependencies.is_none() && graduation.is_none() {
            skill_lifecycle::try_run(&repo, &command_args)
        } else {
            None
        };
        let probation = if dependencies.is_none() && graduation.is_none() && lifecycle.is_none() {
            skill_probation::try_run(&repo, &command_args)
        } else {
            None
        };
        let usage = if dependencies.is_some() {
            Some(skill_dependencies::usage_lines())
        } else if graduation.is_some() {
            Some(skill_graduation::usage_line())
        } else if lifecycle.is_some() {
            Some(skill_lifecycle::usage_lines())
        } else if probation.is_some() {
            Some(skill_probation::usage_line())
        } else {
            None
        };
        let result = match (dependencies, graduation, lifecycle, probation) {
            (Some(result), _, _, _)
            | (_, Some(result), _, _)
            | (_, _, Some(result), _)
            | (_, _, _, Some(result)) => result,
            (None, None, None, None) => skills::run(&skill_args),
        };'''
replace("crates/medusa-cli/src/wrapper.rs", old_router, new_router)

# Enforce lifecycle constraints.
replace(
    "crates/medusa-cli/src/skill_lifecycle.rs",
    "    let recommendation = recommendation_for(root, &parsed.name)?;\n    let active = root.join(ACTIVE_ROOT).join(&parsed.name);",
    '''    let recommendation = recommendation_for(root, &parsed.name)?;
    let active_root = root.join(ACTIVE_ROOT);
    let dependents = medusa_runtime::skill_dependencies::reverse_dependents(&active_root, &parsed.name)?;
    if !dependents.is_empty() {
        return Err(format!(
            "skill `{}` cannot be quarantined while active dependents exist: {}",
            parsed.name,
            dependents.join(", ")
        ));
    }
    let active = active_root.join(&parsed.name);''',
)
replace(
    "crates/medusa-cli/src/skill_lifecycle.rs",
    "    let active = root.join(ACTIVE_ROOT).join(name);\n    if active.exists() {",
    '''    medusa_runtime::skill_dependencies::validate_restorable_skill(
        &root.join(ACTIVE_ROOT),
        &quarantined,
        name,
    )?;
    let active = root.join(ACTIVE_ROOT).join(name);
    if active.exists() {''',
)
replace(
    "crates/medusa-cli/src/skill_graduation.rs",
    "    let lifecycle_path = root.join(ACTIVE_ROOT).join(name).join(LIFECYCLE_FILE);",
    '''    medusa_runtime::skill_dependencies::resolve_project_skill(
        &root.join(ACTIVE_ROOT),
        name,
        64_000,
    )?;
    let lifecycle_path = root.join(ACTIVE_ROOT).join(name).join(LIFECYCLE_FILE);''',
)

# The helper is intentionally deleted by the workflow after successful verification.
