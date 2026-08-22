use medusa_provider::{Message, ToolDefinition};

const DEFAULT_CONTEXT_WINDOW_TOKENS: u64 = 128_000;
const COMPACTION_THRESHOLD_PERCENT: u64 = 85;
const BYTES_PER_ESTIMATED_TOKEN: u64 = 4;
const CONTEXT_SAFETY_MARGIN_TOKENS: u64 = 512;

/// Deterministic action selected before sending a provider request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromptBudgetDecision {
    Proceed,
    Compact,
}

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
        let compaction_threshold_tokens =
            context_window_tokens.saturating_mul(COMPACTION_THRESHOLD_PERCENT) / 100;

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
    pub fn decision(self) -> PromptBudgetDecision {
        if self.requires_compaction() {
            PromptBudgetDecision::Compact
        } else {
            PromptBudgetDecision::Proceed
        }
    }

    #[must_use]
    pub fn requires_compaction(self) -> bool {
        self.estimated_total_tokens >= self.compaction_threshold_tokens
    }

    #[must_use]
    pub fn exceeds_context_window(self) -> bool {
        self.estimated_total_tokens > self.context_window_tokens
    }

    #[must_use]
    pub fn remaining_tokens(self) -> u64 {
        self.context_window_tokens
            .saturating_sub(self.estimated_total_tokens)
    }

    /// Returns a response budget that leaves room for provider-side tokenization overhead.
    ///
    /// The request budget includes the configured response reservation, but providers count
    /// serialized envelopes and tool schemas differently. Keep a small safety margin and never
    /// send a zero-token request after compaction has made the input fit.
    #[must_use]
    pub fn response_token_budget(self, requested: u32) -> u32 {
        let input_tokens = self
            .system_tokens
            .saturating_add(self.conversation_tokens)
            .saturating_add(self.tool_tokens);
        let input_exceeds_context =
            self.exceeds_context_window() && input_tokens > self.context_window_tokens;
        let available = if input_exceeds_context {
            0
        } else if !self.exceeds_context_window() {
            self.remaining_tokens()
                .saturating_add(self.reserved_response_tokens)
                .saturating_sub(CONTEXT_SAFETY_MARGIN_TOKENS)
        } else {
            self.context_window_tokens
                .saturating_sub(input_tokens)
                .saturating_sub(CONTEXT_SAFETY_MARGIN_TOKENS)
        };
        let available = u32::try_from(available).unwrap_or(u32::MAX).max(1);
        requested.max(1).min(available)
    }
}

#[must_use]
pub fn configured_context_window_tokens(configured: u64) -> u64 {
    if configured > 0 {
        configured
    } else {
        DEFAULT_CONTEXT_WINDOW_TOKENS
    }
}

/// Identifies provider errors that should trigger one compact-and-retry cycle.
#[must_use]
pub fn is_context_limit_rejection(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    [
        "context length",
        "context window",
        "maximum context",
        "max context",
        "prompt is too long",
        "too many tokens",
        "token limit",
        "request too large",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn estimate_serialized_tokens<T: serde::Serialize + ?Sized>(value: &T) -> u64 {
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
