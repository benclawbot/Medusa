use medusa_skills::SkillIndex;

#[test]
fn empty_assets_directory_produces_empty_index() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("skills")).unwrap();
    let index = SkillIndex::from_assets_dir(dir.path()).unwrap();
    assert_eq!(index.entries().len(), 0);
}

#[test]
fn first_three_skills_are_vendored() {
    let dir = tempfile::tempdir().unwrap();
    let skills_root = dir.path().join("skills");
    for (name, triggers) in [
        ("brainstorming", "brainstorm,design,idea"),
        ("test-driven-development", "tdd,test,red-green"),
        ("systematic-debugging", "debug,bug,broken"),
    ] {
        let skill = skills_root.join(name);
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            format!("---\nname: {name}\nversion: 1.0.0\ndescription: stub\ntriggers: [{triggers}]\ncompatibility:\n  medusa: '>=1.0.0'\n---\n\n# {name}\nBody.\n"),
        ).unwrap();
    }
    let index = SkillIndex::from_assets_dir(dir.path()).unwrap();
    let names: Vec<&str> = index.entries().iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["brainstorming", "systematic-debugging", "test-driven-development"]);
    let brainstorming = index.by_name("brainstorming").unwrap();
    assert!(brainstorming.manifest.triggers.contains(&"brainstorm".to_owned()));
}

#[test]
fn all_fourteen_skills_are_vendored() {
    let dir = tempfile::tempdir().unwrap();
    let skills_root = dir.path().join("skills");
    for name in [
        "brainstorming",
        "dispatching-parallel-agents",
        "executing-plans",
        "finishing-a-development-branch",
        "receiving-code-review",
        "requesting-code-review",
        "subagent-driven-development",
        "systematic-debugging",
        "test-driven-development",
        "using-git-worktrees",
        "using-superpowers",
        "verification-before-completion",
        "writing-plans",
        "writing-skills",
    ] {
        let skill = skills_root.join(name);
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            format!("---\nname: {name}\nversion: 1.0.0\ndescription: stub\ntriggers: [stub]\ncompatibility:\n  medusa: '>=1.0.0'\n---\n\n# {name}\n"),
        ).unwrap();
    }
    let index = medusa_skills::SkillIndex::from_assets_dir(dir.path()).unwrap();
    assert_eq!(index.entries().len(), 14);
    let names: Vec<&str> = index.entries().iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "brainstorming",
            "dispatching-parallel-agents",
            "executing-plans",
            "finishing-a-development-branch",
            "receiving-code-review",
            "requesting-code-review",
            "subagent-driven-development",
            "systematic-debugging",
            "test-driven-development",
            "using-git-worktrees",
            "using-superpowers",
            "verification-before-completion",
            "writing-plans",
            "writing-skills",
        ],
    );
}
