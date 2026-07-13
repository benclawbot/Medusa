use std::{
    env, fs,
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

const MAX_SKILL_BYTES: usize = 64_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SkillSummary {
    pub name: String,
    pub scope: String,
    pub description: Option<String>,
}

pub(crate) fn summaries(repo: &Path) -> Vec<SkillSummary> {
    let mut skills = roots(repo)
        .into_iter()
        .flat_map(|(scope, root)| entries_for_root(&scope, &root))
        .collect::<Vec<_>>();
    skills.sort_by(|left, right| (&left.scope, &left.name).cmp(&(&right.scope, &right.name)));
    skills.dedup_by(|left, right| left.scope == right.scope && left.name == right.name);
    skills
}

pub(crate) fn read(repo: &Path, name: &str, scope: Option<&str>) -> MedusaResult<String> {
    let name = validate_name(name)?;
    let scope = scope.map(str::trim).filter(|scope| !scope.is_empty());
    for (candidate_scope, root) in roots(repo) {
        if scope.is_some_and(|requested| requested != candidate_scope) {
            continue;
        }
        let path = root.join(name).join("SKILL.md");
        if !path.is_file() {
            continue;
        }
        let content = fs::read_to_string(&path).map_err(|error| {
            MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Execution,
                format!("could not read skill {name}: {error}"),
            )
        })?;
        return Ok(format!(
            "Skill: {name} ({candidate_scope})\nSource: {}\n\n{}",
            path.display(),
            truncate(&content)
        ));
    }
    Err(MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        match scope {
            Some(scope) => format!("skill {name} was not found in {scope} skills"),
            None => format!("skill {name} was not found"),
        },
    ))
}

fn roots(repo: &Path) -> Vec<(String, PathBuf)> {
    let mut roots = vec![
        ("project".to_owned(), repo.join(".medusa/skills")),
        ("project".to_owned(), repo.join(".claude/skills")),
    ];
    if let Some(home) = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
    {
        roots.push(("user".to_owned(), home.join(".medusa/skills")));
        roots.push(("user".to_owned(), home.join(".claude/skills")));
    }
    roots
}

fn entries_for_root(scope: &str, root: &Path) -> Vec<SkillSummary> {
    fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path().join("SKILL.md");
            path.is_file().then(|| SkillSummary {
                name: entry.file_name().to_string_lossy().into_owned(),
                scope: scope.to_owned(),
                description: description(&path),
            })
        })
        .collect()
}

fn description(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().and_then(|content| {
        content.lines().find_map(|line| {
            line.strip_prefix("description:")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.trim_matches('"').to_owned())
        })
    })
}

fn validate_name(name: &str) -> MedusaResult<&str> {
    let name = name.trim();
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\'])
        || name.contains("..")
    {
        return Err(MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            "skill name must be a single directory name",
        ));
    }
    Ok(name)
}

fn truncate(content: &str) -> String {
    if content.len() <= MAX_SKILL_BYTES {
        return content.to_owned();
    }
    let mut end = MAX_SKILL_BYTES;
    while !content.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}\n[truncated]", &content[..end])
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn project_skill_can_be_discovered_and_read() {
        let directory = tempdir().expect("temporary directory");
        let skill = directory.path().join(".claude/skills/release/SKILL.md");
        fs::create_dir_all(skill.parent().expect("skill directory")).expect("create skills");
        fs::write(
            &skill,
            "---\ndescription: Prepare a release\n---\nUse release steps.",
        )
        .expect("write skill");

        assert!(
            summaries(directory.path())
                .iter()
                .any(|skill| { skill.name == "release" && skill.scope == "project" })
        );
        let content = read(directory.path(), "release", Some("project")).expect("read skill");
        assert!(content.contains("Use release steps."));
    }

    #[test]
    fn traversal_names_are_rejected() {
        let directory = tempdir().expect("temporary directory");
        assert!(read(directory.path(), "../secret", None).is_err());
    }
}
