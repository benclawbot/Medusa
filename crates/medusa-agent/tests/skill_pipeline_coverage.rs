use medusa_agent::skill_loader::{SkillBundle, load};
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
    use medusa_extensions::{SkillCompatibility, SkillManifest, SkillPermissions};
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
                compatibility: SkillCompatibility {
                    medusa: ">=1.0.0".into(),
                },
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
    let matches = match_prompt(
        "help me brainstorm a new feature",
        &index,
        &config(ConfigMatcherMode::Keyword, 5),
    )
    .unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].skill.name, "brainstorming");
    assert!(
        matches[0]
            .matched_triggers
            .contains(&"brainstorm".to_owned())
    );
}

#[test]
fn keyword_filter_returns_empty_for_no_match() {
    let index = index_with(&[("brainstorming", &["brainstorm"])]);
    let matches = match_prompt(
        "please run the tests",
        &index,
        &config(ConfigMatcherMode::Keyword, 5),
    )
    .unwrap();
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
    let matches = match_prompt(
        "anything with x",
        &index,
        &config(ConfigMatcherMode::Keyword, 2),
    )
    .unwrap();
    assert_eq!(matches.len(), 2);
}

#[test]
fn keyword_filter_scores_by_trigger_count() {
    let index = index_with(&[("a", &["x"]), ("b", &["x", "y"])]);
    let matches = match_prompt("x and y", &index, &config(ConfigMatcherMode::Keyword, 5)).unwrap();
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].skill.name, "b");
    assert_eq!(matches[0].score, 2.0);
}

fn entry(name: &str, requires: &[&str], handoff: Option<&str>) -> medusa_skills::SkillEntry {
    use medusa_extensions::{SkillCompatibility, SkillManifest, SkillPermissions};
    medusa_skills::SkillEntry {
        name: name.to_owned(),
        manifest: SkillManifest {
            name: name.to_owned(),
            version: "1.0.0".into(),
            description: format!("{name} skill"),
            triggers: vec![],
            tools: vec![],
            permissions: SkillPermissions::default(),
            compatibility: SkillCompatibility {
                medusa: ">=1.0.0".into(),
            },
            tests: vec![],
            requires: vec![],
            handoff: None,
        },
        body: format!("# {name}\n"),
        requires: requires.iter().map(|s| s.to_string()).collect(),
        handoff: handoff.map(str::to_owned),
    }
}

fn make_index(entries: Vec<medusa_skills::SkillEntry>) -> SkillIndex {
    SkillIndex { skills: entries }
}

#[test]
fn loader_resolves_single_skill() {
    let index = make_index(vec![entry("a", &[], None)]);
    let bundle: SkillBundle = load(&index, "a", 4).unwrap();
    assert_eq!(bundle.entries.len(), 1);
    assert_eq!(bundle.entries[0].skill.name, "a");
}

#[test]
fn loader_resolves_chain_in_declaration_order() {
    let index = make_index(vec![
        entry("a", &["b"], None),
        entry("b", &["c"], None),
        entry("c", &[], None),
    ]);
    let bundle = load(&index, "a", 4).unwrap();
    let names: Vec<&str> = bundle
        .entries
        .iter()
        .map(|e| e.skill.name.as_str())
        .collect();
    assert_eq!(names, vec!["a", "b", "c"]);
}

#[test]
fn loader_detects_cycle() {
    let index = make_index(vec![entry("a", &["b"], None), entry("b", &["a"], None)]);
    let err = load(&index, "a", 4).unwrap_err();
    assert!(format!("{err}").contains("cycle"));
}

#[test]
fn loader_enforces_depth_cap() {
    let index = make_index(vec![entry("a", &["b"], None), entry("b", &["a"], None)]);
    let err = load(&index, "a", 1).unwrap_err();
    assert!(format!("{err}").contains("depth"));
}

use medusa_agent::skill_injector::render;

fn bundle_one() -> SkillBundle {
    let index = make_index(vec![entry("a", &[], None)]);
    load(&index, "a", 4).unwrap()
}

fn bundle_chain() -> SkillBundle {
    let index = make_index(vec![entry("a", &["b"], None), entry("b", &[], None)]);
    load(&index, "a", 4).unwrap()
}

#[test]
fn render_marks_loaded_skills_section() {
    let rendered = render(&bundle_one());
    assert!(rendered.contains("## Loaded skills"));
    assert!(rendered.contains("# a"));
}

#[test]
fn render_marks_required_skills() {
    let rendered = render(&bundle_chain());
    assert!(rendered.contains("required by 'a'"));
    assert!(rendered.contains("# b"));
}

#[test]
fn render_handles_empty_bundle() {
    let rendered = render(&SkillBundle::default());
    assert_eq!(rendered, "## Loaded skills\n(none)\n");
}

use medusa_agent::skill_handoff::{HandoffOutcome, HandoffQueue};

#[test]
fn handoff_queue_drains_in_order() {
    let mut q = HandoffQueue::default();
    q.push("a");
    q.push("b");
    assert_eq!(q.pop(), Some("a".to_owned()));
    assert_eq!(q.pop(), Some("b".to_owned()));
    assert_eq!(q.pop(), None);
}

#[test]
fn handoff_outcome_records_skipped_when_handoff_target_missing() {
    let index = make_index(vec![entry("a", &[], Some("missing"))]);
    let mut q = HandoffQueue::default();
    q.push("a");
    let outcome: HandoffOutcome = q.drain(&index);
    assert_eq!(outcome.resolved, vec!["a".to_string()]);
}

use medusa_agent::engine::{TurnInput, build_user_turn_input};

#[test]
fn build_user_turn_input_prepends_loaded_skills() {
    let bundle = bundle_chain();
    let input: TurnInput = build_user_turn_input("help me design a feature", &bundle);
    assert!(input.system_prompt_section.contains("## Loaded skills"));
    assert!(input.user_prompt == "help me design a feature");
}

#[test]
fn session_load_skills_is_idempotent() {
    use medusa_agent::AgentSession;
    use medusa_agent::skill_handoff::HandoffQueue;
    use medusa_core::SessionId;
    use std::path::PathBuf;
    use time::OffsetDateTime;

    let now = OffsetDateTime::now_utc();
    let mut session = AgentSession {
        id: SessionId::new(),
        objective: "test".into(),
        repo: PathBuf::from("."),
        created_at: now,
        updated_at: now,
        completed: false,
        turn: 0,
        plan: vec![],
        pending_question: None,
        messages: vec![],
        events: vec![],
        evidence: vec![],
        skill_index: None,
        skill_handoff: HandoffQueue::default(),
    };

    // First call with no paths is a no-op that must not error or change state.
    session.load_skills(None, None).expect("no-op load");
    assert!(session.skill_index.is_none());

    // Second call replaces nothing because there is no source. Still idempotent.
    session.load_skills(None, None).expect("repeated no-op");
    assert!(session.skill_index.is_none());
}

#[test]
fn force_load_bypasses_matcher() {
    use medusa_agent::engine::force_load;
    let index = make_index(vec![entry("a", &[], None), entry("b", &[], None)]);
    let bundle = force_load(&index, "b", 4).unwrap();
    let names: Vec<&str> = bundle
        .entries
        .iter()
        .map(|e| e.skill.name.as_str())
        .collect();
    assert_eq!(names, vec!["b"]);
}
