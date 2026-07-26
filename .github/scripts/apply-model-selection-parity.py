from pathlib import Path

support = Path("crates/medusa-runtime/src/support.rs")
text = support.read_text()
text = text.replace(
    '"supported providers are minimax, anthropic, and anthropic-compatible".to_owned(),',
    'format!("supported providers are {}", SUPPORTED_PROVIDERS.join(", ")),',
)
text = text.replace(
    "    state.config.model.provider = configuration.provider;\n    state.config.model.name = configuration.model;",
    "    state.config.model.protocol = protocol_for_provider(&configuration.provider).to_owned();\n    state.config.model.provider = configuration.provider;\n    state.config.model.name = configuration.model;",
)
old = '''pub(super) fn is_supported_provider(provider: &str) -> bool {
    matches!(provider, "minimax" | "anthropic" | "anthropic-compatible")
}
'''
new = '''pub(super) const SUPPORTED_PROVIDERS: [&str; 8] = [
    "minimax",
    "anthropic",
    "anthropic-compatible",
    "openai",
    "openai-oauth",
    "openai-compatible",
    "omniroute",
    "local",
];

pub(super) fn is_supported_provider(provider: &str) -> bool {
    SUPPORTED_PROVIDERS.contains(&provider)
}

pub(super) fn protocol_for_provider(provider: &str) -> &'static str {
    match provider {
        "minimax" | "anthropic" | "anthropic-compatible" => "anthropic",
        _ => "openai",
    }
}
'''
assert old in text
text = text.replace(old, new)
text = text.replace(
    '"set provider: /model provider <minimax|anthropic|anthropic-compatible>".to_owned(),',
    'format!("set provider: /model provider <{}>", SUPPORTED_PROVIDERS.join("|")),',
)
old = '''    match provider {
        "minimax" => Some("MINIMAX_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "anthropic-compatible" => Some("MEDUSA_API_KEY"),
        _ => None,
    }
'''
new = '''    match provider {
        "minimax" => Some("MINIMAX_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "anthropic-compatible" | "openai-compatible" => Some("MEDUSA_API_KEY"),
        "openai" => Some("OPENAI_API_KEY"),
        "openai-oauth" | "omniroute" | "local" => None,
        _ => None,
    }
'''
assert old in text
text = text.replace(old, new)
text = text.replace(
    '        assert!(is_supported_provider("anthropic-compatible"));\n        assert!(!is_supported_provider("other"));',
    '        assert!(is_supported_provider("anthropic-compatible"));\n        assert!(is_supported_provider("openai"));\n        assert!(is_supported_provider("openai-oauth"));\n        assert!(is_supported_provider("openai-compatible"));\n        assert!(is_supported_provider("omniroute"));\n        assert!(is_supported_provider("local"));\n        assert!(!is_supported_provider("other"));\n        assert_eq!(protocol_for_provider("anthropic"), "anthropic");\n        assert_eq!(protocol_for_provider("openai"), "openai");',
)
text = text.replace(
    '        assert_eq!(credential_environment("other"), None);',
    '        assert_eq!(credential_environment("openai"), Some("OPENAI_API_KEY"));\n        assert_eq!(credential_environment("openai-compatible"), Some("MEDUSA_API_KEY"));\n        assert_eq!(credential_environment("openai-oauth"), None);\n        assert_eq!(credential_environment("omniroute"), None);\n        assert_eq!(credential_environment("local"), None);\n        assert_eq!(credential_environment("other"), None);',
)
support.write_text(text)

runtime = Path("crates/medusa-runtime/src/lib.rs")
text = runtime.read_text()
text = text.replace(
    "    effort_for_turns, forward_update, is_supported_provider, load_selected_skill, message_blocks,",
    "    effort_for_turns, forward_update, is_supported_provider, load_selected_skill, message_blocks,\n    protocol_for_provider, SUPPORTED_PROVIDERS,",
)
text = text.replace(
    '                        "supported providers are minimax, anthropic, and anthropic-compatible"\n                            .to_owned(),',
    '                        format!("supported providers are {}", SUPPORTED_PROVIDERS.join(", ")),',
)
text = text.replace(
    "                state.config.model.provider = provider;",
    "                state.config.model.protocol = protocol_for_provider(&provider).to_owned();\n                state.config.model.provider = provider;",
)
runtime.write_text(text)

models = Path("crates/medusa-tui/src/app/models.rs")
text = models.read_text()
text = text.replace(
    '    const PROVIDERS: [&str; 3] = ["minimax", "anthropic", "anthropic-compatible"];',
    '''    const PROVIDERS: [&str; 8] = [
        "minimax",
        "anthropic",
        "anthropic-compatible",
        "openai",
        "openai-oauth",
        "openai-compatible",
        "omniroute",
        "local",
    ];''',
)
old = '''        "anthropic" => vec![
            "claude-opus-4-6".to_owned(),
            "claude-sonnet-4-6".to_owned(),
            "claude-haiku-4-5".to_owned(),
        ],
        _ => vec!["custom-model".to_owned()],
'''
new = '''        "anthropic" => vec![
            "claude-opus-4-6".to_owned(),
            "claude-sonnet-4-6".to_owned(),
            "claude-haiku-4-5".to_owned(),
        ],
        "openai" | "openai-oauth" => vec![
            "gpt-5.1-codex".to_owned(),
            "gpt-5.1".to_owned(),
            "gpt-5-mini".to_owned(),
        ],
        "omniroute" => vec!["auto/coding".to_owned()],
        "local" => vec!["local-model".to_owned()],
        _ => vec!["custom-model".to_owned()],
'''
assert old in text
models.write_text(text.replace(old, new))
