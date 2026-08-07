use std::collections::BTreeSet;

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ModelResponse, Usage};

/// Provider-neutral incremental output that can be durably recorded and replayed.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderStreamEvent {
    ResponseStarted {
        response_id: Option<String>,
    },
    TextDelta {
        text: String,
    },
    ToolUseReady {
        id: String,
        name: String,
        input: Value,
    },
    Usage {
        usage: Usage,
    },
    Completed {
        response: ModelResponse,
    },
}

/// Ordered stream event with a deterministic sequence number for transcript replay.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SequencedStreamEvent {
    pub sequence: u64,
    pub event: ProviderStreamEvent,
}

/// Validates and numbers provider events before they enter an authoritative transcript.
#[derive(Debug, Default)]
pub struct ProviderStreamTranscript {
    next_sequence: u64,
    completed: bool,
    ready_tool_ids: BTreeSet<String>,
    events: Vec<SequencedStreamEvent>,
}

impl ProviderStreamTranscript {
    #[must_use]
    pub fn events(&self) -> &[SequencedStreamEvent] {
        &self.events
    }

    pub fn push(&mut self, event: ProviderStreamEvent) -> MedusaResult<u64> {
        if self.completed {
            return Err(stream_error(
                "provider emitted output after stream completion",
            ));
        }
        if let ProviderStreamEvent::ToolUseReady { id, .. } = &event
            && !self.ready_tool_ids.insert(id.clone())
        {
            return Err(stream_error(format!(
                "provider emitted duplicate ready tool call {id}"
            )));
        }
        if matches!(event, ProviderStreamEvent::Completed { .. }) {
            self.completed = true;
        }
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.events.push(SequencedStreamEvent { sequence, event });
        Ok(sequence)
    }

    pub fn replay(
        events: &[SequencedStreamEvent],
        mut sink: impl FnMut(&ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<()> {
        let mut expected = 0_u64;
        let mut completed = false;
        let mut tool_ids = BTreeSet::new();
        for entry in events {
            if entry.sequence != expected {
                return Err(stream_error(format!(
                    "provider stream sequence gap: expected {expected}, got {}",
                    entry.sequence
                )));
            }
            if completed {
                return Err(stream_error("provider stream has events after completion"));
            }
            if let ProviderStreamEvent::ToolUseReady { id, .. } = &entry.event
                && !tool_ids.insert(id.as_str())
            {
                return Err(stream_error(format!(
                    "provider stream replays duplicate tool call {id}"
                )));
            }
            sink(&entry.event)?;
            completed = matches!(entry.event, ProviderStreamEvent::Completed { .. });
            expected = expected.saturating_add(1);
        }
        if !completed {
            return Err(stream_error("provider stream ended without completion"));
        }
        Ok(())
    }
}

fn stream_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Execution,
        message.into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ResponseBlock, Usage};

    fn response() -> ModelResponse {
        ModelResponse {
            response_id: Some("response-1".to_owned()),
            stop_reason: Some("tool_calls".to_owned()),
            blocks: vec![ResponseBlock::ToolUse {
                id: "call-1".to_owned(),
                name: "read_file".to_owned(),
                input: serde_json::json!({"path":"src/lib.rs"}),
            }],
            usage: Usage::default(),
        }
    }

    #[test]
    fn transcript_is_replayable_in_exact_order() {
        let mut transcript = ProviderStreamTranscript::default();
        transcript
            .push(ProviderStreamEvent::ResponseStarted {
                response_id: Some("response-1".to_owned()),
            })
            .expect("start");
        transcript
            .push(ProviderStreamEvent::ToolUseReady {
                id: "call-1".to_owned(),
                name: "read_file".to_owned(),
                input: serde_json::json!({"path":"src/lib.rs"}),
            })
            .expect("tool");
        transcript
            .push(ProviderStreamEvent::Completed {
                response: response(),
            })
            .expect("complete");

        let mut seen = Vec::new();
        ProviderStreamTranscript::replay(transcript.events(), |event| {
            seen.push(event.clone());
            Ok(())
        })
        .expect("replay");
        assert_eq!(seen.len(), 3);
    }

    #[test]
    fn duplicate_tool_ready_and_post_completion_output_fail_closed() {
        let mut transcript = ProviderStreamTranscript::default();
        transcript
            .push(ProviderStreamEvent::ToolUseReady {
                id: "call-1".to_owned(),
                name: "read_file".to_owned(),
                input: serde_json::json!({}),
            })
            .expect("first tool");
        assert!(
            transcript
                .push(ProviderStreamEvent::ToolUseReady {
                    id: "call-1".to_owned(),
                    name: "read_file".to_owned(),
                    input: serde_json::json!({}),
                })
                .is_err()
        );
        transcript
            .push(ProviderStreamEvent::Completed {
                response: response(),
            })
            .expect("complete");
        assert!(
            transcript
                .push(ProviderStreamEvent::TextDelta {
                    text: "late".to_owned()
                })
                .is_err()
        );
    }
}
