use medusa_config::Config;
use medusa_provider::{
    ImageSource, Message, MessageBlock, MiniMaxProvider, ModelProvider, ModelRequest,
    ProviderCapabilities, ResponseBlock, Role, ToolDefinition, Usage,
};
use serde_json::json;

fn config(provider: &str) -> Config {
    let mut config = Config::default();
    config.model.provider = provider.to_owned();
    config.model.name = "coverage-model".to_owned();
    config
}

fn text_request() -> ModelRequest {
    ModelRequest {
        system: "system".to_owned(),
        messages: vec![Message {
            role: Role::User,
            content: vec![MessageBlock::Text {
                text: "hello".to_owned(),
            }],
        }],
        tools: vec![ToolDefinition {
            name: "echo".to_owned(),
            description: "echo input".to_owned(),
            input_schema: json!({"type": "object"}),
        }],
        max_tokens: 256,
        temperature_milli: 250,
    }
}

fn image_request(count: usize) -> ModelRequest {
    let mut request = text_request();
    request.messages[0].content = (0..count)
        .map(|index| MessageBlock::Image {
            source: ImageSource::AttachmentRef {
                attachment_id: format!("image-{index}"),
            },
            alt_text: None,
        })
        .collect();
    request
}

#[test]
fn unsupported_provider_is_rejected() {
    let error = MiniMaxProvider::from_config_with_api_key(
        &config("unknown-provider"),
        Some("session-key".to_owned()),
    )
    .err()
    .expect("unsupported provider error");
    assert!(error.to_string().contains("unsupported provider"));
}

#[test]
fn minimax_and_compatible_defaults_are_text_only() {
    for provider_name in ["minimax", "anthropic-compatible"] {
        let provider = MiniMaxProvider::from_config_with_api_key(
            &config(provider_name),
            Some("session-key".to_owned()),
        )
        .expect("provider");
        assert!(!provider.capabilities().image_input);
    }
}

#[test]
fn anthropic_declares_image_contract() {
    let provider = MiniMaxProvider::from_config_with_api_key(
        &config("anthropic"),
        Some("session-key".to_owned()),
    )
    .expect("provider");
    let capabilities = provider.capabilities();
    assert!(capabilities.image_input);
    assert_eq!(capabilities.max_images_per_request, Some(20));
    assert_eq!(capabilities.max_image_bytes, Some(20 * 1024 * 1024));
    assert!(
        capabilities
            .supported_image_media_types
            .contains(&"image/png".to_owned())
    );
}

#[test]
fn text_only_provider_blocks_images_before_network_access() {
    let provider = MiniMaxProvider::from_config_with_api_key(
        &config("anthropic-compatible"),
        Some("session-key".to_owned()),
    )
    .expect("provider");
    let error = provider
        .complete(&image_request(1))
        .expect_err("image validation should fail");
    assert!(
        error
            .to_string()
            .contains("does not declare image-input support")
    );
}

#[test]
fn anthropic_provider_enforces_image_count_before_network_access() {
    let provider = MiniMaxProvider::from_config_with_api_key(
        &config("anthropic"),
        Some("session-key".to_owned()),
    )
    .expect("provider");
    let error = provider
        .complete(&image_request(21))
        .expect_err("image count validation should fail");
    assert!(error.to_string().contains("exceeding provider limit"));
}

#[test]
fn request_and_message_contracts_round_trip() {
    let request = text_request();
    let value = serde_json::to_value(&request).expect("serialize request");
    assert_eq!(value["temperature_milli"], 250);
    assert_eq!(value["messages"][0]["role"], "user");
    assert_eq!(value["messages"][0]["content"][0]["type"], "text");
    let decoded: ModelRequest = serde_json::from_value(value).expect("deserialize request");
    assert_eq!(decoded, request);
}

#[test]
fn every_message_block_variant_round_trips() {
    let blocks = vec![
        MessageBlock::Text {
            text: "text".to_owned(),
        },
        MessageBlock::Image {
            source: ImageSource::Base64 {
                media_type: "image/jpeg".to_owned(),
                data: "AAEC".to_owned(),
            },
            alt_text: Some("preview".to_owned()),
        },
        MessageBlock::ToolUse {
            id: "tool-1".to_owned(),
            name: "echo".to_owned(),
            input: json!({"value": 1}),
        },
        MessageBlock::ToolResult {
            tool_use_id: "tool-1".to_owned(),
            content: "done".to_owned(),
            is_error: false,
        },
    ];
    for block in blocks {
        let value = serde_json::to_value(&block).expect("serialize block");
        let decoded: MessageBlock = serde_json::from_value(value).expect("deserialize block");
        assert_eq!(decoded, block);
    }
}

#[test]
fn response_blocks_usage_and_capabilities_round_trip() {
    let blocks = vec![
        ResponseBlock::Text {
            text: "answer".to_owned(),
        },
        ResponseBlock::ToolUse {
            id: "tool-2".to_owned(),
            name: "inspect".to_owned(),
            input: json!({"path": "src"}),
        },
    ];
    let blocks_value = serde_json::to_value(&blocks).expect("serialize blocks");
    let decoded_blocks: Vec<ResponseBlock> =
        serde_json::from_value(blocks_value).expect("deserialize blocks");
    assert_eq!(decoded_blocks, blocks);

    let usage = Usage {
        input_tokens: 10,
        output_tokens: 5,
        cache_read_input_tokens: 3,
        cache_creation_input_tokens: 2,
    };
    let decoded_usage: Usage =
        serde_json::from_value(serde_json::to_value(usage).expect("serialize usage"))
            .expect("deserialize usage");
    assert_eq!(decoded_usage, usage);

    let capabilities = ProviderCapabilities {
        image_input: true,
        supported_image_media_types: vec!["image/png".to_owned()],
        max_image_bytes: Some(1024),
        max_images_per_request: Some(2),
    };
    let decoded_capabilities: ProviderCapabilities = serde_json::from_value(
        serde_json::to_value(&capabilities).expect("serialize capabilities"),
    )
    .expect("deserialize capabilities");
    assert_eq!(decoded_capabilities, capabilities);
}
