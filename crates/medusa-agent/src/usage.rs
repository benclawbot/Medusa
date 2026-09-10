use std::{env, fs, path::Path, time::Duration};

use medusa_protocol::EventPayload;
use medusa_provider::{ModelRequest, ModelResponse, ResponseBlock, Usage};
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

/// Typed per-turn cost estimate. The estimator stays heuristic (byte/4 tokens and
/// `MEDUSA_*_COST_MICROUSD_PER_MILLION` rates); the newtype keeps untyped
/// micro-USD integers from crossing trust boundaries unnoticed.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct EstimatedCost(pub u64);

pub fn estimated_cost(usage: &Usage) -> EstimatedCost {
    EstimatedCost(
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
        )),
    )
}

/// Durable per-turn cost record: restart-durable, queryable, and linked to the
/// originating operation so journal, telemetry, and cost views can be joined.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TurnCost {
    pub schema_version: u16,
    pub session_id: String,
    pub turn: u32,
    #[serde(default)]
    pub operation_id: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub total_tokens: u64,
    pub duration_ms: u64,
    pub estimated_cost_microusd: u64,
    pub provenance: UsageProvenance,
    pub recorded_unix_ms: i128,
}

impl TurnCost {
    #[must_use]
    pub fn from_turn(session_id: &str, turn: &TurnUsage, operation_id: &str) -> Self {
        Self {
            schema_version: 1,
            session_id: session_id.to_owned(),
            turn: turn.turn,
            operation_id: operation_id.to_owned(),
            input_tokens: turn.input_tokens,
            output_tokens: turn.output_tokens,
            cache_read_input_tokens: turn.cache_read_input_tokens,
            cache_creation_input_tokens: turn.cache_creation_input_tokens,
            total_tokens: turn.total_tokens,
            duration_ms: turn.duration_ms,
            estimated_cost_microusd: turn.estimated_cost_microusd,
            provenance: turn.provenance,
            recorded_unix_ms: time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000,
        }
    }
}

pub const MAX_COST_LINES: usize = 5_000;

fn cost_ledger_path(repo: &Path) -> std::path::PathBuf {
    repo.join(".medusa").join("turn-costs.jsonl")
}

/// Appends one cost record to the restart-durable ledger and rotates old lines.
pub fn append_turn_cost(repo: &Path, cost: &TurnCost) -> medusa_core::MedusaResult<()> {
    use std::io::Write as _;
    let path = cost_ledger_path(repo);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    serde_json::to_writer(&mut file, cost).map_err(std::io::Error::other)?;
    file.write_all(b"\n")?;
    file.sync_data()?;
    drop(file);
    let _ = prune_cost_ledger(repo);
    Ok(())
}

/// Loads every cost record; missing ledger reads as empty so fresh checkouts work.
pub fn load_turn_costs(repo: &Path) -> medusa_core::MedusaResult<Vec<TurnCost>> {
    let body = match fs::read_to_string(cost_ledger_path(repo)) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut costs = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        costs.push(serde_json::from_str::<TurnCost>(line).map_err(std::io::Error::other)?);
    }
    Ok(costs)
}

/// Queries the ledger for one session turn.
pub fn query_turn_cost(
    repo: &Path,
    session_id: &str,
    turn: u32,
) -> medusa_core::MedusaResult<Option<TurnCost>> {
    Ok(load_turn_costs(repo)?
        .into_iter()
        .find(|cost| cost.session_id == session_id && cost.turn == turn))
}

/// Retention: keeps only the newest cost lines.
pub fn prune_cost_ledger(repo: &Path) -> medusa_core::MedusaResult<usize> {
    let path = cost_ledger_path(repo);
    let body = match fs::read_to_string(&path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    let lines: Vec<&str> = body.lines().collect();
    if lines.len() <= MAX_COST_LINES {
        return Ok(0);
    }
    fs::write(
        &path,
        lines[lines.len() - MAX_COST_LINES..].join("\n") + "\n",
    )?;
    Ok(lines.len() - MAX_COST_LINES)
}

fn estimated_cost_microusd(usage: &Usage) -> u64 {
    estimated_cost(usage).0
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
    use medusa_provider::{Message, MessageBlock, Role, ToolDefinition};
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
    fn typed_cost_ledger_survives_restart_and_is_queryable() {
        let directory = tempfile::tempdir().expect("tempdir");
        assert!(load_turn_costs(directory.path()).expect("load").is_empty());
        let usage = TurnUsage {
            turn: 2,
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
            duration_ms: 100,
            tokens_per_second_milli: 150_000,
            estimated_cost_microusd: 7,
            provenance: UsageProvenance::ProviderReported,
            ..TurnUsage::default()
        };
        let cost = TurnCost::from_turn("session-fixture", &usage, "op-cost-1");
        assert_eq!(cost.estimated_cost_microusd, 7);
        append_turn_cost(directory.path(), &cost).expect("append");
        let loaded = load_turn_costs(directory.path()).expect("reload");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].operation_id, "op-cost-1");
        let queried = query_turn_cost(directory.path(), "session-fixture", 2)
            .expect("query")
            .expect("found");
        assert_eq!(queried.total_tokens, 15);
        assert!(
            query_turn_cost(directory.path(), "session-fixture", 3)
                .expect("query")
                .is_none()
        );
    }

    #[test]
    fn cost_estimator_output_is_typed() {
        let usage = medusa_provider::Usage {
            input_tokens: 8,
            output_tokens: 4,
            ..medusa_provider::Usage::default()
        };
        let typed = estimated_cost(&usage);
        assert_eq!(typed.0, estimated_cost_microusd(&usage));
        assert_eq!(typed, EstimatedCost(estimated_cost_microusd(&usage)));
        let zero = estimated_cost(&medusa_provider::Usage::default());
        assert_eq!(zero, EstimatedCost(0));
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
                request_id: None,
                request_fingerprint: None,
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
            codex_thread_id: None,
            world_model: None,
        };
        let aggregate = session_usage(&session);
        assert_eq!(aggregate.turns.len(), 1);
        assert_eq!(aggregate.total_tokens, 15);
        assert_eq!(aggregate.duration_ms, 100);
    }
}
