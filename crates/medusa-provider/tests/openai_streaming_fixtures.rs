use medusa_provider::{OpenAiStreamAccumulator, ProviderStreamEvent, ResponseBlock};

#[test]
fn fragmented_tool_call_becomes_ready_before_stream_completion() {
    let mut accumulator = OpenAiStreamAccumulator::default();
    let mut events = Vec::new();
    let mut sink = |event| {
        events.push(event);
        Ok(())
    };

    accumulator
        .push_sse_data(
            r#"{"id":"resp-1","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"fs_read","arguments":"{\"pa"}}]},"finish_reason":null}],"usage":null}"#,
            &mut sink,
        )
        .expect("first tool fragment");
    accumulator
        .push_sse_data(
            r#"{"id":"resp-1","choices":[{"delta":{"tool_calls":[{"index":0,"id":null,"function":{"name":null,"arguments":"th\":\"README.md\"}"}}]},"finish_reason":"tool_calls"}],"usage":null}"#,
            &mut sink,
        )
        .expect("second tool fragment");

    let ready_position = events
        .iter()
        .position(|event| matches!(event, ProviderStreamEvent::ToolUseReady { .. }))
        .expect("tool use ready event");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ProviderStreamEvent::Completed { .. })),
        "completion must not be emitted until the provider terminates the stream"
    );

    let response = accumulator
        .push_sse_data("[DONE]", &mut sink)
        .expect("done marker")
        .expect("completed response");
    let completed_position = events
        .iter()
        .position(|event| matches!(event, ProviderStreamEvent::Completed { .. }))
        .expect("completed event");

    assert!(ready_position < completed_position);
    assert_eq!(response.blocks.len(), 1);
    match &response.blocks[0] {
        ResponseBlock::ToolUse { id, name, input } => {
            assert_eq!(id, "call-1");
            assert_eq!(name, "fs_read");
            assert_eq!(input["path"], "README.md");
        }
        block => panic!("expected tool use block, got {block:?}"),
    }
}

#[test]
fn malformed_stream_chunk_fails_closed_without_terminal_event() {
    let mut accumulator = OpenAiStreamAccumulator::default();
    let mut events = Vec::new();
    let mut sink = |event| {
        events.push(event);
        Ok(())
    };

    let error = accumulator
        .push_sse_data("{not-json", &mut sink)
        .expect_err("malformed provider JSON must fail closed");

    assert!(error.to_string().contains("invalid JSON"));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ProviderStreamEvent::Completed { .. }))
    );
}

#[test]
fn done_marker_rejects_incomplete_fragmented_tool_call() {
    let mut accumulator = OpenAiStreamAccumulator::default();
    let mut events = Vec::new();
    let mut sink = |event| {
        events.push(event);
        Ok(())
    };

    accumulator
        .push_sse_data(
            r#"{"id":"resp-incomplete","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-incomplete","function":{"name":"fs_read","arguments":"{\"path\":"}}]},"finish_reason":null}],"usage":null}"#,
            &mut sink,
        )
        .expect("partial tool call");

    let error = accumulator
        .push_sse_data("[DONE]", &mut sink)
        .expect_err("incomplete tool call must reject stream completion");

    assert!(error.to_string().contains("completion boundary"));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ProviderStreamEvent::ToolUseReady { .. }))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ProviderStreamEvent::Completed { .. }))
    );
}

#[test]
fn data_after_done_marker_is_rejected() {
    let mut accumulator = OpenAiStreamAccumulator::default();
    let mut events = Vec::new();
    let mut sink = |event| {
        events.push(event);
        Ok(())
    };

    accumulator
        .push_sse_data(
            r#"{"id":"resp-done","choices":[{"delta":{"content":"ok","tool_calls":[]},"finish_reason":"stop"}],"usage":null}"#,
            &mut sink,
        )
        .expect("content chunk");
    accumulator
        .push_sse_data("[DONE]", &mut sink)
        .expect("done marker")
        .expect("completed response");

    let error = accumulator
        .push_sse_data(
            r#"{"id":"resp-done","choices":[],"usage":null}"#,
            &mut sink,
        )
        .expect_err("post-completion output must be rejected");

    assert!(error.to_string().contains("after completion"));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ProviderStreamEvent::Completed { .. }))
            .count(),
        1
    );
}
