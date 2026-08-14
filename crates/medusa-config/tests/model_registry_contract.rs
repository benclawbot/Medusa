use medusa_config::{model_capabilities, model_registry, provider_model_options};

#[test]
fn compatibility_model_options_follow_canonical_registry_ordering() {
    let discovered = vec!["gpt-live".to_owned(), "gpt-5-mini".to_owned()];
    let options = provider_model_options("openai", "private-model", &discovered);
    let discovered_models = discovered
        .iter()
        .map(|id| medusa_config::DiscoveredModel {
            id: id.clone(),
            display_name: None,
        })
        .collect::<Vec<_>>();
    let registry = model_registry(
        "openai",
        "private-model",
        Ok(&discovered_models),
        None,
        0,
    );
    let registry_ids = registry
        .models
        .iter()
        .map(|model| model.id.clone())
        .collect::<Vec<_>>();

    assert_eq!(options, registry_ids);
}

#[test]
fn realtime_preflight_metadata_requires_both_audio_and_realtime() {
    let realtime = model_capabilities("openai-oauth", "gpt-realtime");
    assert!(realtime.audio_input);
    assert!(realtime.realtime);

    let text = model_capabilities("openai-oauth", "gpt-5");
    assert!(text.audio_input);
    assert!(!text.realtime);
}

#[test]
fn image_and_tool_capabilities_are_model_metadata() {
    let anthropic = model_capabilities("anthropic", "claude-sonnet-4-6");
    assert!(anthropic.image_input);
    assert!(anthropic.tool_calling);

    let minimax = model_capabilities("minimax", "MiniMax-M3");
    assert!(!minimax.image_input);
    assert!(minimax.tool_calling);
}
