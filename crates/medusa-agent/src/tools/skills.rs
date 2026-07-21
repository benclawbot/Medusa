use std::{
    env, fs,
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

const MAX_SKILL_BYTES: usize = 64_000;
const MAX_AUTOMATIC_SKILLS: usize = 8;
const MAX_AUTOMATIC_SKILL_BYTES: usize = 24_000;
const MAX_SINGLE_AUTOMATIC_SKILL_BYTES: usize = 8_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SkillSummary {
    pub name: String,
    pub scope: String,
    pub description: Option<String>,
    automatic_instructions: Option<String>,
}

pub(crate) fn summaries(repo: &Path) -> Vec<SkillSummary> {
    let mut skills = roots(repo)
        .into_iter()
        .flat_map(|(scope, root, automatic)| entries_for_root(&scope, &root, automatic))
        .collect::<Vec<_>>();
    skills.sort_by(|left, right| (&left.scope, &left.name).cmp(&(&right.scope, &right.name)));
    skills.dedup_by(|left, right| left.scope == right.scope && left.name == right.name);
    attach_approved_instructions(&mut skills);
    skills
}

pub(crate) fn read(repo: &Path, name: &str, scope: Option<&str>) -> MedusaResult<String> {
    let name = validate_name(name)?;
    let scope = scope.map(str::trim).filter(|scope| !scope.is_empty());
    for (candidate_scope, root, _) in roots(repo) {
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

fn roots(repo: &Path) -> Vec<(String, PathBuf, bool)> {
    let mut roots = vec![
        ("project".to_owned(), repo.join(".medusa/skills"), true),
        ("project".to_owned(), repo.join(".claude/skills"), false),
    ];
    if let Some(home) = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
    {
        roots.push(("user".to_owned(), home.join(".medusa/skills"), false));
        roots.push(("user".to_owned(), home.join(".claude/skills"), false));
    }
    roots
}

fn entries_for_root(scope: &str, root: &Path, automatic: bool) -> Vec<SkillSummary> {
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
                automatic_instructions: automatic
                    .then(|| fs::read_to_string(&path).ok())
                    .flatten(),
            })
        })
        .collect()
}

fn attach_approved_instructions(skills: &mut [SkillSummary]) {
    let mut remaining = MAX_AUTOMATIC_SKILL_BYTES;
    let mut included = 0;
    for skill in skills {
        let Some(content) = skill.automatic_instructions.take() else {
            continue;
        };
        if included >= MAX_AUTOMATIC_SKILLS || remaining == 0 {
            continue;
        }
        let budget = remaining.min(MAX_SINGLE_AUTOMATIC_SKILL_BYTES);
        let instructions = truncate_to_bytes(&content, budget);
        if instructions.is_empty() {
            continue;
        }
        let prefix = skill
            .description
            .take()
            .map(|description| format!("{description}\n\n"))
            .unwrap_or_default();
        let loaded = format!(
            "{prefix}Approved instructions automatically loaded:\n{instructions}"
        );
        remaining = remaining.saturating_sub(instructions.len());
        skill.description = Some(loaded);
        included += 1;
    }
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
    format!("{}\n[truncated]", truncate_to_bytes(content, MAX_SKILL_BYTES))
}

fn truncate_to_bytes(content: &str, max_bytes: usize) -> &str {
    if content.len() <= max_bytes {
        return content;
    }
    let mut end = max_bytes;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    &content[..end]
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
    fn approved_medusa_skill_is_automatically_loaded() {
        let directory = tempdir().expect("temporary directory");
        let skill = directory.path().join(".medusa/skills/verify/SKILL.md");
        fs::create_dir_all(skill.parent().expect("skill directory")).expect("create skills");
        fs::write(
            &skill,
            "---\ndescription: Verify changes\n---\nAlways run targeted tests.",
        )
        .expect("write skill");

        let summary = summaries(directory.path())
            .into_iter()
            .find(|skill| skill.name == "verify")
            .expect("approved skill");
        let description = summary.description.expect("loaded instructions");
        assert!(description.contains("Approved instructions automatically loaded:"));
        assert!(description.contains("Always run targeted tests."));
    }

    #[test]
    fn claude_skill_remains_on_demand_only() {
        let directory = tempdir().expect("temporary directory");
        let skill = directory.path().join(".claude/skills/release/SKILL.md");
        fs::create_dir_all(skill.parent().expect("skill directory")).expect("create skills");
        fs::write(
            &skill,
            "---\ndescription: Prepare a release\n---\nSecret on-demand steps.",
        )
        .expect("write skill");

        let summary = summaries(directory.path())
            .into_iter()
            .find(|skill| skill.name == "release")
            .expect("claude skill");
        assert_eq!(summary.description.as_deref(), Some("Prepare a release"));
    }

    #[test]
    fn automatic_loading_is_bounded_and_utf8_safe() {
        let directory = tempdir().expect("temporary directory");
        for index in 0..12 {
            let skill = directory
                .path()
                .join(format!(".medusa/skills/skill-{index:02}/SKILL.md"));
            fs::create_dir_all(skill.parent().expect("skill directory"))
                .expect("create skills");
            fs::write(&skill, "é".repeat(MAX_SINGLE_AUTOMATIC_SKILL_BYTES))
                .expect("write skill");
        }

        let summaries = summaries(directory.path());
        let loaded = summaries
            .iter()
            .filter(|skill| {
                skill.description.as_deref().is_some_and(|description| {
                    description.contains("Approved instructions automatically loaded:")
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(loaded.len(), MAX_AUTOMATIC_SKILLS);
        let loaded_bytes = loaded
            .iter()
            .map(|skill| skill.description.as_deref().unwrap_or_default().len())
            .sum::<usize>();
        assert!(loaded_bytes <= MAX_AUTOMATIC_SKILL_BYTES + 512);
    }

    #[test]
    fn traversal_names_are_rejected() {
        let directory = tempdir().expect("temporary directory");
        assert!(read(directory.path(), "../secret", None).is_err());
    }
}
