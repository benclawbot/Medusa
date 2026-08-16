from pathlib import Path

repls = {
    'crates/medusa-runtime/src/checkpoint_store.rs': [(
'''            &EventPayload::ModelRequestStarted {\n                provider: "provider".to_owned(),\n                model: "model".to_owned(),\n            }''',
'''            &EventPayload::ModelRequestStarted {\n                provider: "provider".to_owned(),\n                model: "model".to_owned(),\n                request_id: None,\n                request_fingerprint: None,\n                manifest_ref: None,\n                attempt_ordinal: 0,\n                parent_request_id: None,\n            }''')],
    'crates/medusa-runtime/src/execution_history.rs': [(
'''        EventPayload::ModelResponseReceived { .. } => "model_response_received",\n        EventPayload::ProviderExecutionRecorded { .. } => "provider_execution_recorded",''',
'''        EventPayload::ModelResponseReceived { .. } => "model_response_received",\n        EventPayload::ModelRequestFailed { .. } => "model_request_failed",\n        EventPayload::ProviderExecutionRecorded { .. } => "provider_execution_recorded",''')],
    'crates/medusa-runtime/src/tests.rs': [(
'''        &AgentUpdate::Event(EventPayload::ModelRequestStarted {\n            provider: "minimax".to_owned(),\n            model: "MiniMax-M3".to_owned(),\n        }),''',
'''        &AgentUpdate::Event(EventPayload::ModelRequestStarted {\n            provider: "minimax".to_owned(),\n            model: "MiniMax-M3".to_owned(),\n            request_id: None,\n            request_fingerprint: None,\n            manifest_ref: None,\n            attempt_ordinal: 0,\n            parent_request_id: None,\n        }),'''),(
'''        &AgentUpdate::Event(EventPayload::ModelResponseReceived {\n            response_id: Some("legacy-response".to_owned()),\n            usage: json!({''',
'''        &AgentUpdate::Event(EventPayload::ModelResponseReceived {\n            response_id: Some("legacy-response".to_owned()),\n            request_id: None,\n            request_fingerprint: None,\n            usage: json!({'''),(
'''        &AgentUpdate::Event(EventPayload::ModelResponseReceived {\n            response_id: Some("normalized-response".to_owned()),\n            usage: json!({''',
'''        &AgentUpdate::Event(EventPayload::ModelResponseReceived {\n            response_id: Some("normalized-response".to_owned()),\n            request_id: None,\n            request_fingerprint: None,\n            usage: json!({''')],
}

for filename, pairs in repls.items():
    p = Path(filename)
    s = p.read_text()
    for old, new in pairs:
        if old not in s:
            raise SystemExit(f'missing expected text in {filename}: {old[:80]!r}')
        s = s.replace(old, new, 1)
    p.write_text(s)
