use std::env;

use medusa_provider::{Message, ToolDefinition};

const DEFAULT_CONTEXT_WINDOW_TOKENS: u64 = 128_000;
const COMPACTION_THRESHOLD_PERCENT: u64 = 85;
const BYTES_PER_ESTIMATED_TOKEN: u64 = 4;

/// Deterministic, provider-neutral estimate of how one request consumes context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PromptBudget {
    pub context_window_tokens: u64,
    pub system_tokens: u64,
    pub conversation_tokens: u64,
    pub tool_tokens: u64,
    pub reserved_response_tokens: u64,
    pub estimated_total_tokens: u64,
    pub compaction_threshold_tokens: u64,
}

impl PromptBudget {
    #[must_use]
    pub fn for_request(
        system: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        reserved_response_tokens: u32,
        context_window_tokens: u64,
    ) -> Self {
        let system_tokens = estimate_text_tokens(system);
        let conversation_tokens = estimate_serialized_tokens(messages);
        let tool_tokens = estimate_serialized_tokens(tools);
        let reserved_response_tokens = u64::from(reserved_response_tokens);
        let estimated_total_tokens = system_tokens
            .saturating_add(conversation_tokens)
            .saturating_add(tool_tokens)
            .saturating_add(reserved_response_tokens);
        let compaction_threshold_tokens = context_window_tokens
            .saturating_mul(COMPACTION_THRESHOLD_PERCENT)
            / 100;

        Self {
            context_window_tokens,
            system_tokens,
            conversation_tokens,
            tool_tokens,
            reserved_response_tokens,
            estimated_total_tokens,
            compaction_threshold_tokens,
        }
    }

    #[must_use]
    pub fn requires_compaction(self) -> bool {
        self.estimated_total_tokens >= self.compaction_threshold_tokens
    }
}

#[must_use]
pub fn configured_context_window_tokens() -> u64 {
    env::var("MEDUSA_CONTEXT_WINDOW_TOKENS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_CONTEXT_WINDOW_TOKENS)
}

fn estimate_serialized_tokens<T: serde::Serialize>(value: &T) -> u64 {
    serde_json::to_vec(value)
        .map(|bytes| estimate_bytes_tokens(bytes.len()))
        .unwrap_or(u64::MAX)
}

fn estimate_text_tokens(value: &str) -> u64 {
    estimate_bytes_tokens(value.len())
}

fn estimate_bytes_tokens(bytes: usize) -> u64 {
    u64::try_from(bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(BYTES_PER_ESTIMATED_TOKEN - 1)
        / BYTES_PER_ESTIMATED_TOKEN
}
