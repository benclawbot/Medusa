from pathlib import Path

P = Path('crates/medusa-agent/src/engine.rs')
s = P.read_text()

def rep(old, new):
    global s
    if new in s:
        return
    if old not in s:
        raise SystemExit('missing fragment:\n' + old[:300])
    s = s.replace(old, new, 1)

rep(
'''fn duration_ns(duration: std::time::Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}
''',
'''fn duration_ns(duration: std::time::Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

struct EarlyToolResult {
    name: String,
    input: serde_json::Value,
    result: MedusaResult<String>,
    timing: ToolExecutionTiming,
}

fn early_stream_tool_allowed(name: &str, input: &serde_json::Value) -> bool {
    if !matches!(name, "fs_read" | "fs_search" | "web_search" | "web_fetch") {
        return false;
    }
    let profile = crate::tool_dag::profile(name, input);
    profile.side_effect == crate::tool_dag::SideEffectClass::None
        && profile.idempotent
        && profile.parallel_safe
}

fn early_stream_tool_mismatch(id: &str) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Execution,
        format!("streamed tool call {id} did not match the terminal provider response"),
    )
}
''')

rep(
'''        let streaming = self.provider.capabilities().streaming;
        let mut stream_transcript = ProviderStreamTranscript::default();
        let mut streamed_text = String::new();
        let mut stream_text_rejected = false;
        let mut complete_request = |request: &ModelRequest| {
''',
'''        let streaming = self.provider.capabilities().streaming;
        let mut stream_transcript = ProviderStreamTranscript::default();
        let mut streamed_text = String::new();
        let mut stream_text_rejected = false;
        let mut tool_requested_at = BTreeMap::<String, std::time::Instant>::new();
        let mut early_tool_results = BTreeMap::<String, EarlyToolResult>::new();
        let response = {
        let mut complete_request = |request: &ModelRequest| {
''')

rep(
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
                Ok(())
''',
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
                        if early_stream_tool_allowed(&name, &input)
                            && self.execution_policy.denial_reason(&name, &input).is_none()
                            && tool_allowed(self.config.agent.mode, &name) =>
                    {
                        let requested_at = std::time::Instant::now();
                        tool_requested_at.insert(id.clone(), requested_at);
                        append_observed(
                            session,
                            EventPayload::ToolCallRequested {
                                tool: audited_tool_name(&name, &input),
                                arguments: input.clone(),
                            },
                            &mut observer,
                        )?;
                        append_observed(
                            session,
                            EventPayload::ToolExecutionStarted {
                                tool: audited_tool_name(&name, &input),
                            },
                            &mut observer,
                        )?;
                        let started = std::time::Instant::now();
                        let result = execute_tool_cancellable(
                            &session.repo,
                            &name,
                            &input,
                            self.cancellation.as_ref(),
                        );
                        let timing = ToolExecutionTiming {
                            queue_duration_ns: duration_ns(started.duration_since(requested_at)),
                            execution_duration_ns: duration_ns(started.elapsed()),
                            cached: false,
                        };
                        early_tool_results.insert(
                            id,
                            EarlyToolResult { name, input, result, timing },
                        );
                    }
                    _ => {}
                }
                Ok(())
''')

rep(
'''        let response = match complete_request(&request) {
''',
'''        match complete_request(&request) {
''')
rep(
'''            Err(error) => return Err(error),
        };
        let turn_usage = crate::session::record_turn_usage(
''',
'''            Err(error) => return Err(error),
        }
        };
        let turn_usage = crate::session::record_turn_usage(
''')

rep(
'''        let mut calls = VecDeque::new();
        let mut tool_requested_at = BTreeMap::<String, std::time::Instant>::new();
        for block in response.blocks {
''',
'''        let mut calls = VecDeque::new();
        for block in response.blocks {
''')

rep(
'''                ResponseBlock::ToolUse { id, name, input } => {
                    assistant_blocks.push(MessageBlock::ToolUse {
''',
'''                ResponseBlock::ToolUse { id, name, input } => {
                    if let Some(early) = early_tool_results.get(&id) {
                        if early.name != name || early.input != input {
                            return Err(early_stream_tool_mismatch(&id));
                        }
                    } else {
                        tool_requested_at.insert(id.clone(), std::time::Instant::now());
                    }
                    assistant_blocks.push(MessageBlock::ToolUse {
''')
rep(
'''                    tool_requested_at.insert(id.clone(), std::time::Instant::now());
                    calls.push_back((id, name, input));
''',
'''                    calls.push_back((id, name, input));
''')

rep(
'''        if !assistant_blocks.is_empty() {
''',
'''        if early_tool_results.keys().any(|early_id| {
            !calls.iter().any(|(id, _, _)| id == early_id)
        }) {
            let id = early_tool_results.keys().next().cloned().unwrap_or_default();
            return Err(early_stream_tool_mismatch(&id));
        }
        if !assistant_blocks.is_empty() {
''')

rep(
'''            let positions = crate::tool_dag::select_ready_positions(
                &schedulable,
                parallel_tool_limit(self.config.agent.parallel_workers),
            );
            let batch = crate::tool_dag::drain_positions(&mut calls, &positions);
''',
'''            let mut positions = crate::tool_dag::select_ready_positions(
                &schedulable,
                parallel_tool_limit(self.config.agent.parallel_workers),
            );
            if let Some(position) = positions.iter().copied().find(|position| {
                calls
                    .get(*position)
                    .is_some_and(|(id, _, _)| early_tool_results.contains_key(id))
            }) {
                positions = vec![position];
            }
            let batch = crate::tool_dag::drain_positions(&mut calls, &positions);
''')

rep(
'''            for (_, name, input) in &batch {
                append_observed(
''',
'''            for (id, name, input) in &batch {
                if early_tool_results.contains_key(id) {
                    continue;
                }
                append_observed(
''')

rep(
'''                let started = std::time::Instant::now();
                let queue_duration_ns = tool_requested_at
''',
'''                let early_result = early_tool_results.remove(&id);
                let started = std::time::Instant::now();
                let queue_duration_ns = tool_requested_at
''')
rep(
'''                let mut measured = false;
                let mut cached = false;
                let result = if let Some(reason) =
                    self.execution_policy.denial_reason(&name, &input)
                {
''',
'''                let mut measured = false;
                let mut cached = false;
                let mut early_timing = None;
                let result = if let Some(early) = early_result {
                    early_timing = Some(early.timing);
                    early.result
                } else if let Some(reason) = self.execution_policy.denial_reason(&name, &input) {
''')
rep(
'''                let timing = measured.then(|| ToolExecutionTiming {
                    queue_duration_ns,
                    execution_duration_ns: duration_ns(started.elapsed()),
                    cached,
                });
''',
'''                let timing = early_timing.or_else(|| measured.then(|| ToolExecutionTiming {
                    queue_duration_ns,
                    execution_duration_ns: duration_ns(started.elapsed()),
                    cached,
                }));
''')

rep(
'''    #[test]
    fn truncated_provider_output_does_not_complete_the_turn() {
        assert_eq!(read_only_step("length"), StepOutcome::Continue);
    }
}''',
'''    #[test]
    fn truncated_provider_output_does_not_complete_the_turn() {
        assert_eq!(read_only_step("length"), StepOutcome::Continue);
    }

    #[test]
    fn only_explicit_read_only_tools_are_eligible_for_early_stream_dispatch() {
        assert!(early_stream_tool_allowed("fs_read", &serde_json::json!({"path":"a"})));
        assert!(early_stream_tool_allowed("fs_search", &serde_json::json!({"query":"x"})));
        assert!(early_stream_tool_allowed("web_search", &serde_json::json!({"query":"x"})));
        assert!(early_stream_tool_allowed("web_fetch", &serde_json::json!({"url":"https://example.com"})));
        assert!(!early_stream_tool_allowed("fs_write", &serde_json::json!({"path":"a","content":"x"})));
        assert!(!early_stream_tool_allowed("shell_run", &serde_json::json!({"program":"git","args":["status"]})));
    }
}''')

P.write_text(s)
