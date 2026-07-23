#[path = "../src/context_budget.rs"]
mod context_budget;

use context_budget::PromptBudget;
use medusa_provider::{Message, MessageBlock, Role, ToolDefinition};
use serde_json::json;

fn message(text: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![MessageBlock::Text {
            text: text.to_owned(),
        }],
    }
}

#[test]
fn request_budget_exposes_stable_allocations() {
    let tools = vec![ToolDefinition {
        name: "fs_read".to_owned(),
        description: "Read one file".to_owned(),
        input_schema: json!({"type": "object"}),
    }];
    let budget = PromptBudget::for_request(
        "system prompt",
        &[message("inspect the repository")],
        &tools,
        4_096,
        128_000,
    );

    assert!(budget.system_tokens > 0);
    assert!(budget.conversation_tokens > 0);
    assert!(budget.tool_tokens > 0);
    assert_eq!(budget.reserved_response_tokens, 4_096);
    assert_eq!(budget.compaction_threshold_tokens, 108_800);
    assert_eq!(
        budget.estimated_total_tokens,
        budget.system_tokens
            + budget.conversation_tokens
            + budget.tool_tokens
            + budget.reserved_response_tokens
    );
}

#[test]
fn compaction_boundary_is_deterministic() {
    let below = PromptBudget::for_request("", &[], &[], 84, 100);
    let at_boundary = PromptBudget::for_request("", &[], &[], 85, 100);

    assert!(!below.requires_compaction());
    assert!(at_boundary.requires_compaction());
}

#[test]
fn configured_window_has_a_non_zero_default() {
    assert!(context_budget::configured_context_window_tokens() > 0);
}
