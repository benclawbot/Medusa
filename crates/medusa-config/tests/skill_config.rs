#![allow(unsafe_code)]

use medusa_config::SkillConfig;

#[test]
#[serial_test::serial]
fn defaults_when_no_env_set() {
    unsafe {
        std::env::remove_var("MEDUSA_SKILLS_ENABLED");
        std::env::remove_var("MEDUSA_SKILLS_BUNDLE_PATH");
        std::env::remove_var("MEDUSA_SKILLS_MAX_MATCHES");
        std::env::remove_var("MEDUSA_SKILLS_MAX_CHAIN_DEPTH");
        std::env::remove_var("MEDUSA_SKILLS_MATCHER_MODE");
    }
    let cfg = SkillConfig::from_env();
    assert!(cfg.enabled);
    assert!(cfg.bundle_path.is_none());
    assert_eq!(cfg.max_matches, 5);
    assert_eq!(cfg.max_chain_depth, 4);
    assert_eq!(
        cfg.matcher_mode,
        medusa_config::MatcherMode::KeywordLlmRerank
    );
}

#[test]
#[serial_test::serial]
fn overrides_when_env_set() {
    unsafe {
        std::env::set_var("MEDUSA_SKILLS_ENABLED", "false");
        std::env::set_var("MEDUSA_SKILLS_BUNDLE_PATH", "/opt/skills");
        std::env::set_var("MEDUSA_SKILLS_MAX_MATCHES", "8");
        std::env::set_var("MEDUSA_SKILLS_MAX_CHAIN_DEPTH", "6");
        std::env::set_var("MEDUSA_SKILLS_MATCHER_MODE", "keyword");
    }
    let cfg = SkillConfig::from_env();
    assert!(!cfg.enabled);
    assert_eq!(
        cfg.bundle_path.as_deref(),
        Some(std::path::Path::new("/opt/skills"))
    );
    assert_eq!(cfg.max_matches, 8);
    assert_eq!(cfg.max_chain_depth, 6);
    assert_eq!(cfg.matcher_mode, medusa_config::MatcherMode::Keyword);
    unsafe {
        std::env::remove_var("MEDUSA_SKILLS_ENABLED");
        std::env::remove_var("MEDUSA_SKILLS_BUNDLE_PATH");
        std::env::remove_var("MEDUSA_SKILLS_MAX_MATCHES");
        std::env::remove_var("MEDUSA_SKILLS_MAX_CHAIN_DEPTH");
        std::env::remove_var("MEDUSA_SKILLS_MATCHER_MODE");
    }
}
