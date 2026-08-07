use std::collections::BTreeMap;

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde_json::Value;

use crate::ProviderStreamEvent;

#[derive(Clone, Debug, Default)]
struct PartialToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
    completed: bool,
}

/// Assembles fragmented provider tool calls and emits a ready event only on an explicit finish.
///
/// A syntactically valid JSON prefix is never treated as executable while more provider fragments
/// may still arrive. This keeps early dispatch fail-closed across fragmented streaming protocols.
#[derive(Debug, Default)]
pub struct StreamingToolCallAssembler {
    calls: BTreeMap<u32, PartialToolCall>,
}

impl StreamingToolCallAssembler {
    pub fn push_fragment(
        &mut self,
        index: u32,
        id: Option<&str>,
        name: Option<&str>,
        arguments: &str,
    ) -> MedusaResult<()> {
        let call = self.calls.entry(index).or_default();
        if call.completed {
            return Err(tool_stream_error(format!(
                "provider emitted tool call fragment after completion at index {index}"
            )));
        }
        merge_stable_field(&mut call.id, id, "id", index)?;
        merge_stable_field(&mut call.name, name, "name", index)?;
        call.arguments.push_str(arguments);
        Ok(())
    }

    pub fn finish(&mut self, index: u32) -> MedusaResult<ProviderStreamEvent> {
        let call = self.calls.get_mut(&index).ok_or_else(|| {
            tool_stream_error(format!(
                "provider completed unknown tool call at index {index}"
            ))
        })?;
        if call.completed {
            return Err(tool_stream_error(format!(
                "provider completed tool call twice at index {index}"
            )));
        }
        let id = call.id.clone().ok_or_else(|| {
            tool_stream_error(format!("provider tool call {index} is missing an id"))
        })?;
        let name = call.name.clone().ok_or_else(|| {
            tool_stream_error(format!("provider tool call {index} is missing a name"))
        })?;
        let input: Value = serde_json::from_str(&call.arguments).map_err(|error| {
            tool_stream_error(format!(
                "provider tool call {index} completed with invalid JSON arguments: {error}"
            ))
        })?;
        call.completed = true;
        Ok(ProviderStreamEvent::ToolUseReady { id, name, input })
    }
}

fn merge_stable_field(
    current: &mut Option<String>,
    incoming: Option<&str>,
    field: &str,
    index: u32,
) -> MedusaResult<()> {
    let Some(incoming) = incoming else {
        return Ok(());
    };
    if let Some(current) = current {
        if current != incoming {
            return Err(tool_stream_error(format!(
                "provider changed tool call {field} at index {index}"
            )));
        }
    } else {
        *current = Some(incoming.to_owned());
    }
    Ok(())
}

fn tool_stream_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Execution,
        message.into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragmented_arguments_dispatch_only_after_explicit_finish() {
        let mut assembler = StreamingToolCallAssembler::default();
        assembler
            .push_fragment(
                0,
                Some("call-1"),
                Some("read_file"),
                "{\"path\":\"src/",
            )
            .expect("first fragment");
        assembler
            .push_fragment(0, None, None, "lib.rs\"}")
            .expect("second fragment");

        assert_eq!(
            assembler.finish(0).expect("ready event"),
            ProviderStreamEvent::ToolUseReady {
                id: "call-1".to_owned(),
                name: "read_file".to_owned(),
                input: serde_json::json!({"path":"src/lib.rs"}),
            }
        );
    }

    #[test]
    fn valid_json_prefix_is_not_implicitly_completed() {
        let mut assembler = StreamingToolCallAssembler::default();
        assembler
            .push_fragment(0, Some("call-1"), Some("tool"), "{}")
            .expect("valid prefix");
        assembler
            .push_fragment(0, None, None, " ")
            .expect("trailing fragment remains allowed before finish");
        assert!(assembler.finish(0).is_ok());
    }

    #[test]
    fn malformed_completed_arguments_fail_closed() {
        let mut assembler = StreamingToolCallAssembler::default();
        assembler
            .push_fragment(0, Some("call-1"), Some("tool"), "{\"path\":")
            .expect("fragment");
        assert!(assembler.finish(0).is_err());
    }

    #[test]
    fn conflicting_identity_and_late_fragments_are_rejected() {
        let mut assembler = StreamingToolCallAssembler::default();
        assembler
            .push_fragment(0, Some("call-1"), Some("tool"), "{}")
            .expect("initial");
        assert!(
            assembler
                .push_fragment(0, Some("call-2"), None, "")
                .is_err()
        );
        assembler.finish(0).expect("finish");
        assert!(assembler.push_fragment(0, None, None, " ").is_err());
    }
}
