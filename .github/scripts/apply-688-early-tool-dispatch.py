from pathlib import Path


def replace(path: str, old: str, new: str) -> None:
    target = Path(path)
    text = target.read_text()
    if new in text:
        return
    if old not in text:
        raise SystemExit(f"expected source fragment missing in {path}")
    target.write_text(text.replace(old, new, 1))


ENGINE = "crates/medusa-agent/src/engine.rs"

replace(
    ENGINE,
    '''#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ToolExecutionTiming {
    queue_duration_ns: u64,
    execution_duration_ns: u64,
    cached: bool,
}
''',
    '''#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ToolExecutionTiming {
    queue_duration_ns: u64,
    execution_duration_ns: u64,
    cached: bool,
}

#[derive(Debug)]
struct EarlyToolExecution {
    name: String,
    input: serde_json::Value,
    output: String,
    requested_at: std::time::Instant,
    timing: ToolExecutionTiming,
}

fn stream_dispatch_safe_tool(name: &str, input: &serde_json::Value) -> bool {
    let profile = crate::tool_dag::profile(name, input);
    profile.side_effect == crate::tool_dag::SideEffectClass::None
        && profile.idempotent
        && profile.parallel_safe
        && profile.cancellation_supported
}

fn early_tool_identity_error(id: &str) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Execution,
        format!("streamed tool call {id} changed before provider completion"),
    )
}
''',
)

replace(
    ENGINE,
    '''        let mut stream_transcript = ProviderStreamTranscript::default();
        let mut streamed_text = String::new();
        let mut stream_text_rejected = false;
        let mut complete_request = |request: &ModelRequest| {''',
    '''        let mut stream_transcript = ProviderStreamTranscript::default();
        let mut streamed_text = String::new();
        let mut stream_text_rejected = false;
        let mut early_tool_executions = BTreeMap::<String, EarlyToolExecution>::new();
        let mut complete_request = |request: &ModelRequest| {''',
)

replace(
    ENGINE,
    '''                if let ProviderStreamEvent::TextDelta { text } = event
                    && !stream_text_rejected
                {
                    streamed_text.push_str(&text);
                    if validate_provider_text(&streamed_text).is_ok() {
                        observer(&AgentUpdate::AssistantText(text));
                    } else {
                        stream_text_rejected = true;
                        observer(&AgentUpdate::AssistantText(
                            "[provider output rejected: identity or policy contamination]"
                                .to_owned(),
                        ));
                    }
                }
                Ok(())''',
    '''                match event {
                    ProviderStreamEvent::TextDelta { text } if !stream_text_rejected => {
                        streamed_text.push_str(&text);
                        if validate_provider_text(&streamed_text).is_ok() {
                            observer(&AgentUpdate::AssistantText(text));
                        } else {
                            stream_text_rejected = true;
                            observer(&AgentUpdate::AssistantText(
                                "[provider output rejected: identity or policy contamination]"
                                    .to_owned(),
                            ));
                        }
                    }
                    ProviderStreamEvent::ToolUseReady { id, name, input }
                        if !early_tool_executions.contains_key(&id)
                            && stream_dispatch_safe_tool(&name, &input)
                            && self.execution_policy.denial_reason(&name, &input).is_none()
                            && tool_allowed(self.config.agent.mode, &name) =>
                    {
                        let requested_at = std::time::Instant::now();
                        let started = std::time::Instant::now();
                        if let Ok(output) = execute_tool_cancellable(
                            &session.repo,
                            &name,
                            &input,
                            self.cancellation.as_ref(),
                        ) {
                            early_tool_executions.insert(
                                id,
                                EarlyToolExecution {
                                    name,
                                    input,
                                    output,
                                    requested_at,
                                    timing: ToolExecutionTiming {
                                        queue_duration_ns: 0,
                                        execution_duration_ns: duration_ns(started.elapsed()),
                                        cached: false,
                                    },
                                },
                            );
                        }
                    }
                    _ => {}
                }
                Ok(())''',
)

replace(
    ENGINE,
    '''                ResponseBlock::ToolUse { id, name, input } => {
                    assistant_blocks.push(MessageBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    });
                    tool_requested_at.insert(id.clone(), std::time::Instant::now());
                    calls.push_back((id, name, input));
                }''',
    '''                ResponseBlock::ToolUse { id, name, input } => {
                    if let Some(early) = early_tool_executions.get(&id)
                        && (early.name != name || early.input != input)
                    {
                        return Err(early_tool_identity_error(&id));
                    }
                    assistant_blocks.push(MessageBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    });
                    let requested_at = early_tool_executions
                        .get(&id)
                        .map_or_else(std::time::Instant::now, |early| early.requested_at);
                    tool_requested_at.insert(id.clone(), requested_at);
                    calls.push_back((id, name, input));
                }''',
)

replace(
    ENGINE,
    '''            let positions = crate::tool_dag::select_ready_positions(
                &schedulable,
                parallel_tool_limit(self.config.agent.parallel_workers),
            );''',
    '''            let positions = calls
                .iter()
                .position(|(id, _, _)| early_tool_executions.contains_key(id))
                .map_or_else(
                    || {
                        crate::tool_dag::select_ready_positions(
                            &schedulable,
                            parallel_tool_limit(self.config.agent.parallel_workers),
                        )
                    },
                    |position| vec![position],
                );''',
)

replace(
    ENGINE,
    '''                let mut measured = false;
                let mut cached = false;
                let result = if let Some(reason) =
                    self.execution_policy.denial_reason(&name, &input)
                {''',
    '''                let mut measured = false;
                let mut cached = false;
                let mut timing_override = None;
                let result = if let Some(reason) =
                    self.execution_policy.denial_reason(&name, &input)
                {''',
)

replace(
    ENGINE,
    '''                    Err(MedusaError::new(
                        ErrorCode::PolicyDenied,
                        ErrorCategory::Policy,
                        reason,
                    ))
                } else if name == "update_plan" {''',
    '''                    Err(MedusaError::new(
                        ErrorCode::PolicyDenied,
                        ErrorCategory::Policy,
                        reason,
                    ))
                } else if let Some(early) = early_tool_executions.remove(&id) {
                    if early.name != name || early.input != input {
                        return Err(early_tool_identity_error(&id));
                    }
                    measured = true;
                    timing_override = Some(early.timing);
                    append_observed(
                        session,
                        EventPayload::ToolExecutionStarted {
                            tool: audited_tool_name(&name, &input),
                        },
                        &mut observer,
                    )?;
                    Ok(early.output)
                } else if name == "update_plan" {''',
)

replace(
    ENGINE,
    '''                let timing = measured.then(|| ToolExecutionTiming {
                    queue_duration_ns,
                    execution_duration_ns: duration_ns(started.elapsed()),
                    cached,
                });''',
    '''                let timing = timing_override.or_else(|| {
                    measured.then(|| ToolExecutionTiming {
                        queue_duration_ns,
                        execution_duration_ns: duration_ns(started.elapsed()),
                        cached,
                    })
                });''',
)

# Append behavioral coverage before the terminal stop-reason tests.
replace(
    ENGINE,
    '''#[cfg(test)]
mod terminal_stop_reason_tests {''',
    '''#[cfg(test)]
mod streaming_tool_dispatch_tests {
    use std::{fs, path::PathBuf, sync::atomic::AtomicBool};

    use medusa_provider::{
        ModelResponse, ProviderCapabilities, ProviderStreamEvent, ResponseBlock, Usage,
    };
    use serde_json::json;

    use super::*;

    struct DeletingStreamingProvider {
        path: PathBuf,
    }

    impl ModelProvider for DeletingStreamingProvider {
        fn complete(&self, _request: &ModelRequest) -> MedusaResult<ModelResponse> {
            unreachable!("streaming path must be used")
        }

        fn complete_streaming_cancellable(
            &self,
            _request: &ModelRequest,
            _cancel: &AtomicBool,
            sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
        ) -> MedusaResult<ModelResponse> {
            let input = json!({"path": "streamed.txt"});
            sink(ProviderStreamEvent::ResponseStarted {
                response_id: Some("stream-dispatch".to_owned()),
            })?;
            sink(ProviderStreamEvent::ToolUseReady {
                id: "read-early".to_owned(),
                name: "fs_read".to_owned(),
                input: input.clone(),
            })?;
            fs::remove_file(&self.path).expect("remove source after ready event");
            let response = ModelResponse {
                response_id: Some("stream-dispatch".to_owned()),
                stop_reason: Some("tool_use".to_owned()),
                blocks: vec![ResponseBlock::ToolUse {
                    id: "read-early".to_owned(),
                    name: "fs_read".to_owned(),
                    input,
                }],
                usage: Usage::default(),
            };
            sink(ProviderStreamEvent::Completed {
                response: response.clone(),
            })?;
            Ok(response)
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                tool_calling: true,
                streaming: true,
                ..ProviderCapabilities::default()
            }
        }
    }

    #[test]
    fn safe_tool_executes_when_ready_before_provider_completion() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let path = directory.path().join("streamed.txt");
        fs::write(&path, "before-stream-complete").expect("stream fixture");
        let engine = AgentEngine::new(DeletingStreamingProvider { path }, Config::default());
        let mut session = engine
            .create_session(directory.path(), "read streamed.txt".to_owned())
            .expect("create session");
        let mut observed = Vec::new();
        engine
            .step_with_observer(&mut session, |update| observed.push(update.clone()))
            .expect("streaming step");
        assert!(observed.iter().any(|update| matches!(
            update,
            AgentUpdate::ToolOutput { tool, output, is_error: false }
                if tool == "fs_read" && output.contains("before-stream-complete")
        )));
    }
}

#[cfg(test)]
mod terminal_stop_reason_tests {''',
)
