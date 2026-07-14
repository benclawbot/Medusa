use medusa_agent::skill_matcher::match_prompt;
use medusa_config::{MatcherMode as ConfigMatcherMode, SkillConfig};
use medusa_skills::SkillIndex;

fn config(mode: ConfigMatcherMode, max: usize) -> SkillConfig {
    SkillConfig {
        enabled: true,
        bundle_path: None,
        max_matches: max,
        max_chain_depth: 4,
        matcher_mode: mode,
    }
}

fn index_with(triggers: &[(&str, &[&str])]) -> SkillIndex {
    use medusa_extensions::skills::{SkillCompatibility, SkillManifest, SkillPermissions};
    let skills = triggers
        .iter()
        .map(|(name, ts)| medusa_skills::SkillEntry {
            name: (*name).to_owned(),
            manifest: SkillManifest {
                name: (*name).to_owned(),
                version: "1.0.0".into(),
                description: format!("{name} skill"),
                triggers: ts.iter().map(|s| s.to_string()).collect(),
                tools: vec![],
                permissions: SkillPermissions::default(),
                compatibility: SkillCompatibility { medusa: ">=1.0.0".into() },
                tests: vec![],
                requires: vec![],
                handoff: None,
            },
            body: format!("# {name}\n"),
            requires: vec![],
            handoff: None,
        })
        .collect();
    SkillIndex { skills }
}

#[test]
fn keyword_filter_matches_one_skill() {
    let index = index_with(&[("brainstorming", &["brainstorm", "design"])]);
    let matches = match_prompt("help me brainstorm a new feature", &index, &config(ConfigMatcherMode::Keyword, 5)).unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].skill.name, "brainstorming");
    assert!(matches[0].matched_triggers.contains(&"brainstorm".to_owned()));
}

#[test]
fn keyword_filter_returns_empty_for_no_match() {
    let index = index_with(&[("brainstorming", &["brainstorm"])]);
    let matches = match_prompt("please run the tests", &index, &config(ConfigMatcherMode::Keyword, 5)).unwrap();
    assert!(matches.is_empty());
}

#[test]
fn keyword_filter_caps_at_max_matches() {
    let index = index_with(&[
        ("a", &["x"]),
        ("b", &["x"]),
        ("c", &["x"]),
        ("d", &["x"]),
        ("e", &["x"]),
    ]);
    let matches = match_prompt("anything with x", &index, &config(ConfigMatcherMode::Keyword, 2)).unwrap();
    assert_eq!(matches.len(), 2);
}

#[test]
fn keyword_filter_scores_by_trigger_count() {
    let index = index_with(&[
        ("a", &["x"]),
        ("b", &["x", "y"]),
    ]);
    let matches = match_prompt("x and y", &index, &config(ConfigMatcherMode::Keyword, 5)).unwrap();
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].skill.name, "b");
    assert_eq!(matches[0].score, 2.0);
}