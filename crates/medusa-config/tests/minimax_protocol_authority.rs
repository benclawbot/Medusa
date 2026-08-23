use medusa_config::{Config, ProviderProfile, provider_runtime_protocol};

#[test]
fn minimax_protocol_is_consistent_across_default_profile_catalog_and_dogfood() {
    let default = Config::default();
    assert_eq!(default.model.provider, "minimax");
    assert_eq!(default.model.protocol, "anthropic");

    let profile = ProviderProfile {
        configured: true,
        ..ProviderProfile::default()
    };
    assert_eq!(profile.provider, "minimax");
    assert_eq!(profile.protocol(), "anthropic");

    assert_eq!(provider_runtime_protocol("minimax"), Some("anthropic"));

    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("../../../docs/provider-support.json"))
            .expect("provider support manifest");
    let minimax = manifest["providers"]
        .as_array()
        .expect("providers")
        .iter()
        .find(|provider| provider["id"] == "minimax")
        .expect("minimax support entry");

    assert_eq!(minimax["runtime_protocol"], "anthropic");
    assert_eq!(minimax["dogfood"]["protocol"], "anthropic");
    assert_eq!(
        minimax["dogfood"]["base_url"],
        "https://api.minimax.io/anthropic"
    );
}
