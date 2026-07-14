use crate::skill_loader::SkillBundle;

pub fn render(bundle: &SkillBundle) -> String {
    if bundle.entries.is_empty() {
        return "## Loaded skills\n(none)\n".to_owned();
    }
    let mut out = String::from("## Loaded skills (matched by trigger)\n\n");
    for (index, entry) in bundle.entries.iter().enumerate() {
        let label = if index == 0 {
            format!(
                "- [{}] triggers: {:?}",
                entry.skill.name, entry.skill.manifest.triggers
            )
        } else {
            format!(
                "- [{}] (required by '{}')",
                entry.skill.name,
                bundle.entries[index - 1].skill.name
            )
        };
        out.push_str(&label);
        out.push('\n');
        out.push_str(&entry.skill.body);
        out.push('\n');
    }
    out
}
