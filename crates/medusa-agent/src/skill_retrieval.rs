use std::{fs, path::Path};

const ACTIVE_SKILLS_ROOT: &str = ".medusa/skills";
const MAX_AUTOMATIC_SKILLS: usize = 8;
const MAX_AUTOMATIC_SKILL_BYTES: usize = 24_000;
const MAX_SINGLE_SKILL_BYTES: usize = 8_000;

pub(crate) fn approved_skill_context(repo: &Path) -> String {
    let root = repo.join(ACTIVE_SKILLS_ROOT);
    let Ok(entries) = fs::read_dir(root) else {
        return String::new();
    };

    let mut skills = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_owned();
            let path = entry.path().join("SKILL.md");
            path.is_file().then_some((name, path))
        })
        .collect::<Vec<_>>();
    skills.sort_by(|left, right| left.0.cmp(&right.0));

    let mut context = String::new();
    let mut included = 0;
    for (name, path) in skills {
        if included >= MAX_AUTOMATIC_SKILLS || context.len() >= MAX_AUTOMATIC_SKILL_BYTES {
            break;
        }
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        let content = truncate_utf8(&content, MAX_SINGLE_SKILL_BYTES);
        let header = format!("\n\n### Approved skill: {name}\n");
        let remaining = MAX_AUTOMATIC_SKILL_BYTES.saturating_sub(context.len());
        if header.len() >= remaining {
            break;
        }
        context.push_str(&header);
        let remaining = MAX_AUTOMATIC_SKILL_BYTES.saturating_sub(context.len());
        context.push_str(truncate_utf8(content, remaining));
        included += 1;
    }
    context
}

fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(root: &Path, name: &str, content: &str) {
        let directory = root.join(ACTIVE_SKILLS_ROOT).join(name);
        fs::create_dir_all(&directory).expect("skill directory");
        fs::write(directory.join("SKILL.md"), content).expect("skill content");
    }

    #[test]
    fn approved_skills_are_loaded_in_deterministic_order() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_skill(temp.path(), "zeta", "zeta instructions");
        write_skill(temp.path(), "alpha", "alpha instructions");

        let context = approved_skill_context(temp.path());
        let alpha = context.find("Approved skill: alpha").expect("alpha");
        let zeta = context.find("Approved skill: zeta").expect("zeta");
        assert!(alpha < zeta);
        assert!(context.contains("alpha instructions"));
        assert!(context.contains("zeta instructions"));
    }

    #[test]
    fn unrelated_files_and_invalid_utf8_are_ignored() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join(ACTIVE_SKILLS_ROOT);
        fs::create_dir_all(&root).expect("root");
        fs::write(root.join("README.md"), "not a skill").expect("readme");
        let invalid = root.join("invalid");
        fs::create_dir_all(&invalid).expect("invalid directory");
        fs::write(invalid.join("SKILL.md"), [0xff, 0xfe]).expect("invalid utf8");

        assert!(approved_skill_context(temp.path()).is_empty());
    }

    #[test]
    fn retrieval_is_bounded_and_utf8_safe() {
        let temp = tempfile::tempdir().expect("tempdir");
        for index in 0..12 {
            write_skill(
                temp.path(),
                &format!("skill-{index:02}"),
                &"é".repeat(MAX_SINGLE_SKILL_BYTES),
            );
        }

        let context = approved_skill_context(temp.path());
        assert!(context.len() <= MAX_AUTOMATIC_SKILL_BYTES);
        assert!(std::str::from_utf8(context.as_bytes()).is_ok());
        assert_eq!(context.matches("### Approved skill:").count(), MAX_AUTOMATIC_SKILLS);
    }
}
