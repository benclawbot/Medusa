#[path = "../src/context_budget.rs"]
mod context_budget;

use context_budget::{PromptBudget, PromptBudgetDecision};
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

    assert_eq!(below.decision(), PromptBudgetDecision::Proceed);
    assert_eq!(at_boundary.decision(), PromptBudgetDecision::Compact);
    assert!(!below.requires_compaction());
    assert!(at_boundary.requires_compaction());
}

#[test]
fn remaining_capacity_saturates_after_overflow() {
    let within = PromptBudget::for_request("", &[], &[], 40, 100);
    let beyond = PromptBudget::for_request("", &[], &[], 101, 100);

    assert_eq!(within.remaining_tokens(), 60);
    assert!(!within.exceeds_context_window());
    assert_eq!(beyond.remaining_tokens(), 0);
    assert!(beyond.exceeds_context_window());
}

#[test]
fn provider_context_rejections_are_detected_without_matching_unrelated_errors() {
    for message in [
        "maximum context length exceeded",
        "Prompt is too long for this model",
        "request rejected: too many tokens",
        "context window limit reached",
    ] {
        assert!(context_budget::is_context_limit_rejection(message));
    }

    assert!(!context_budget::is_context_limit_rejection(
        "provider authentication failed"
    ));
    assert!(!context_budget::is_context_limit_rejection(
        "tool execution timed out"
    ));
}

#[test]
fn configured_window_has_a_non_zero_default() {
    assert!(context_budget::configured_context_window_tokens() > 0);
}
