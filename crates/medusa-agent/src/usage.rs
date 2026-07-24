use std::{env, time::Duration};

use medusa_protocol::EventPayload;
use medusa_provider::{MessageBlock, ModelRequest, ModelResponse, ResponseBlock, Usage};
use serde::{Deserialize, Serialize};

use crate::session::AgentSession;

/// Whether token counts came from the provider or Medusa's deterministic estimator.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageProvenance {
    ProviderReported,
    Estimated,
}

impl Default for UsageProvenance {
    fn default() -> Self {
        Self::Estimated
    }
}

/// Usage and performance telemetry for one successful model turn.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TurnUsage {
    pub turn: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub total_tokens: u64,
    pub duration_ms: u64,
    pub tokens_per_second_milli: u64,
    pub estimated_cost_microusd: u64,
    pub provenance: UsageProvenance,
}

/// Cumulative usage reconstructed from a durable session event stream.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionUsage {
    pub turns: Vec<TurnUsage>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub total_tokens: u64,
    pub duration_ms: u64,
    pub estimated_cost_microusd: u64,
}

impl SessionUsage {
    fn push(&mut self, turn: TurnUsage) {
        self.input_tokens = self.input_tokens.saturating_add(turn.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(turn.output_tokens);
        self.cache_read_input_tokens = self
            .cache_read_input_tokens
            .saturating_add(turn.cache_read_input_tokens);
        self.cache_creation_input_tokens = self
            .cache_creation_input_tokens
            .saturating_add(turn.cache_creation_input_tokens);
        self.total_tokens = self.total_tokens.saturating_add(turn.total_tokens);
        self.duration_ms = self.duration_ms.saturating_add(turn.duration_ms);
        self.estimated_cost_microusd = self
            .estimated_cost_microusd
            .saturating_add(turn.estimated_cost_microusd);
        self.turns.push(turn);
    }
}

/// Reconstructs cumulative usage from normalized model-response events.
#[must_use]
pub fn session_usage(session: &AgentSession) -> SessionUsage {
    let mut aggregate = SessionUsage::default();
    for event in &session.events {
        let EventPayload::ModelResponseReceived { usage, .. } = &event.payload else {
            continue;
        };
        if let Ok(turn) = serde_json::from_value::<TurnUsage>(usage.clone()) {
            aggregate.push(turn);
        }
    }
    aggregate
}

pub(crate) fn record_turn_usage(
    turn: u32,
    request: &ModelRequest,
    response: &ModelResponse,
    elapsed: Duration,
) -> TurnUsage {
    let (usage, provenance) = normalized_usage(request, response);
    let total_tokens = usage
        .input_tokens
        .saturating_add(usage.output_tokens)
        .saturating_add(usage.cache_read_input_tokens)
        .saturating_add(usage.cache_creation_input_tokens);
    let duration_ms = elapsed.as_millis().try_into().unwrap_or(u64::MAX);
    let tokens_per_second_milli = if duration_ms == 0 {
        0
    } else {
        total_tokens.saturating_mul(1_000_000) / duration_ms
    };
    TurnUsage {
        turn,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_input_tokens: usage.cache_read_input_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
        total_tokens,
        duration_ms,
        tokens_per_second_milli,
        estimated_cost_microusd: estimated_cost_microusd(&usage),
        provenance,
    }
}

fn normalized_usage(request: &ModelRequest, response: &ModelResponse) -> (Usage, UsageProvenance) {
    let reported = response.usage;
    if reported.input_tokens > 0
        || reported.output_tokens > 0
        || reported.cache_read_input_tokens > 0
        || reported.cache_creation_input_tokens > 0
    {
        return (reported, UsageProvenance::ProviderReported);
    }
    (
        Usage {
            input_tokens: estimate_request_tokens(request),
            output_tokens: estimate_response_tokens(response),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        },
        UsageProvenance::Estimated,
    )
}

fn estimate_request_tokens(request: &ModelRequest) -> u64 {
    serde_json::to_vec(request)
        .map(|bytes| estimate_bytes(bytes.len()))
        .unwrap_or(u64::MAX)
}

fn estimate_response_tokens(response: &ModelResponse) -> u64 {
    let bytes = response.blocks.iter().fold(0_usize, |total, block| {
        let block_bytes = match block {
            ResponseBlock::Text { text } => text.len(),
            ResponseBlock::ToolUse { id, name, input } => id
                .len()
                .saturating_add(name.len())
                .saturating_add(input.to_string().len()),
        };
        total.saturating_add(block_bytes)
    });
    estimate_bytes(bytes)
}

fn estimate_bytes(bytes: usize) -> u64 {
    let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
    bytes.saturating_add(3) / 4
}

fn estimated_cost_microusd(usage: &Usage) -> u64 {
    cost_component(
        usage.input_tokens,
        rate("MEDUSA_INPUT_COST_MICROUSD_PER_MILLION"),
    )
    .saturating_add(cost_component(
        usage.output_tokens,
        rate("MEDUSA_OUTPUT_COST_MICROUSD_PER_MILLION"),
    ))
    .saturating_add(cost_component(
        usage.cache_read_input_tokens,
        rate("MEDUSA_CACHE_READ_COST_MICROUSD_PER_MILLION"),
    ))
    .saturating_add(cost_component(
        usage.cache_creation_input_tokens,
        rate("MEDUSA_CACHE_WRITE_COST_MICROUSD_PER_MILLION"),
    ))
}

fn rate(name: &str) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
}

fn cost_component(tokens: u64, microusd_per_million: u64) -> u64 {
    tokens
        .saturating_mul(microusd_per_million)
        .saturating_add(999_999)
        / 1_000_000
}

#[cfg(test)]
mod tests {
    use super::*;
    use medusa_core::SessionId;
    use medusa_protocol::{Actor, EventEnvelope};
    use medusa_provider::{Message, Role, ToolDefinition};
    use serde_json::json;
    use time::OffsetDateTime;

    fn request() -> ModelRequest {
        ModelRequest {
            system: "system".to_owned(),
            messages: vec![Message {
                role: Role::User,
                content: vec![MessageBlock::Text {
                    text: "hello".to_owned(),
                }],
            }],
            tools: vec![ToolDefinition {
                name: "read".to_owned(),
                description: "read a file".to_owned(),
                input_schema: json!({"type": "object"}),
            }],
            max_tokens: 100,
            temperature_milli: 0,
        }
    }

    #[test]
    fn provider_usage_remains_authoritative() {
        let response = ModelResponse {
            response_id: None,
            stop_reason: None,
            blocks: Vec::new(),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_input_tokens: 2,
                cache_creation_input_tokens: 1,
            },
        };
        let (usage, provenance) = normalized_usage(&request(), &response);
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(provenance, UsageProvenance::ProviderReported);
    }

    #[test]
    fn absent_provider_usage_is_estimated_deterministically() {
        let response = ModelResponse {
            response_id: None,
            stop_reason: None,
            blocks: vec![ResponseBlock::Text {
                text: "12345678".to_owned(),
            }],
            usage: Usage::default(),
        };
        let (first, provenance) = normalized_usage(&request(), &response);
        let (second, _) = normalized_usage(&request(), &response);
        assert_eq!(first, second);
        assert_eq!(first.output_tokens, 2);
        assert_eq!(provenance, UsageProvenance::Estimated);
    }

    #[test]
    fn cumulative_usage_is_reconstructed_from_events() {
        let directory = tempfile::tempdir().expect("tempdir");
        let id = SessionId::new();
        let usage = TurnUsage {
            turn: 1,
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
            duration_ms: 100,
            tokens_per_second_milli: 150_000,
            provenance: UsageProvenance::ProviderReported,
            ..TurnUsage::default()
        };
        let event = EventEnvelope::new(
            0,
            id.clone(),
            Actor::Coordinator,
            medusa_core::CorrelationId::new(),
            EventPayload::ModelResponseReceived {
                response_id: Some("fixture".to_owned()),
                usage: serde_json::to_value(usage).expect("usage json"),
            },
            None,
            OffsetDateTime::now_utc(),
        )
        .expect("event");
        let session = AgentSession {
            id,
            objective: "test".to_owned(),
            repo: directory.path().to_path_buf(),
            created_at: OffsetDateTime::now_utc(),
            updated_at: OffsetDateTime::now_utc(),
            completed: false,
            turn: 1,
            plan: Vec::new(),
            pending_question: None,
            messages: Vec::new(),
            events: vec![event],
            evidence: Vec::new(),
            tool_artifacts: Vec::new(),
            approval_grants: Vec::new(),
            approval_receipts: Vec::new(),
            rollback_receipts: Vec::new(),
        };
        let aggregate = session_usage(&session);
        assert_eq!(aggregate.turns.len(), 1);
        assert_eq!(aggregate.total_tokens, 15);
        assert_eq!(aggregate.duration_ms, 100);
    }
}
