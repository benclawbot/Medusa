from pathlib import Path
import re


def replace_once(text: str, old: str, new: str, label: str) -> str:
    if text.count(old) != 1:
        raise SystemExit(f"{label}: expected exactly one match, found {text.count(old)}")
    return text.replace(old, new, 1)

support_path = Path("crates/medusa-runtime/src/support.rs")
support = support_path.read_text()
support = replace_once(
    support,
    "    sync::mpsc::Sender,\n};",
    "    sync::mpsc::Sender,\n    time::Instant,\n};",
    "support Instant import",
)
support = replace_once(
    support,
    "    RuntimeActivity, RuntimeActivityKind, RuntimeError, RuntimeEvent, RuntimeState, TurnUsage,\n};",
    "    RuntimeActivity, RuntimeActivityKind, RuntimeError, RuntimeEvent, RuntimeState, TurnUsage,\n    UsageProvenance,\n};",
    "support provenance import",
)
support = replace_once(
    support,
    "    pending_tools: VecDeque<PendingTool>,\n    pub(super) current_context_tokens: u64,",
    "    pending_tools: VecDeque<PendingTool>,\n    model_started_at: Option<Instant>,\n    pub(super) current_context_tokens: u64,",
    "support timer field",
)
support = replace_once(
    support,
    "            pending_tools: VecDeque::new(),\n            current_context_tokens: 0,",
    "            pending_tools: VecDeque::new(),\n            model_started_at: None,\n            current_context_tokens: 0,",
    "support timer init",
)
old_branch = '''        AgentUpdate::Event(EventPayload::ModelResponseReceived { usage, .. }) => {
            let Ok(usage) = serde_json::from_value::<TurnUsage>(usage.clone()) else {
                return;
            };
            state.current_context_tokens = usage
                .input_tokens
                .saturating_add(usage.cache_read_input_tokens)
                .saturating_add(usage.cache_creation_input_tokens);
            let _ = events.send(RuntimeEvent::Usage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_read_input_tokens: usage.cache_read_input_tokens,
                cache_creation_input_tokens: usage.cache_creation_input_tokens,
                total_tokens: usage.total_tokens,
                duration_ms: usage.duration_ms,
                tokens_per_second_milli: usage.tokens_per_second_milli,
                estimated_cost_microusd: usage.estimated_cost_microusd,
                provenance: usage.provenance,
            });
        }
'''
new_branch = '''        AgentUpdate::Event(EventPayload::ModelRequestStarted { .. }) => {
            state.model_started_at = Some(Instant::now());
        }
        AgentUpdate::Event(EventPayload::ModelResponseReceived { usage, .. }) => {
            let measured_duration_ms = state.model_started_at.take().map_or(0, |started_at| {
                u64::try_from(started_at.elapsed().as_millis())
                    .unwrap_or(u64::MAX)
                    .max(1)
            });
            let usage = serde_json::from_value::<TurnUsage>(usage.clone()).unwrap_or_else(|_| {
                let input_tokens = usage
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                let output_tokens = usage
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                let cache_read_input_tokens = usage
                    .get("cache_read_input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                let cache_creation_input_tokens = usage
                    .get("cache_creation_input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                let total_tokens = input_tokens
                    .saturating_add(output_tokens)
                    .saturating_add(cache_read_input_tokens)
                    .saturating_add(cache_creation_input_tokens);
                TurnUsage {
                    turn: 0,
                    input_tokens,
                    output_tokens,
                    cache_read_input_tokens,
                    cache_creation_input_tokens,
                    total_tokens,
                    duration_ms: measured_duration_ms,
                    tokens_per_second_milli: if measured_duration_ms == 0 {
                        0
                    } else {
                        total_tokens.saturating_mul(1_000_000) / measured_duration_ms
                    },
                    estimated_cost_microusd: 0,
                    provenance: if total_tokens == 0 {
                        UsageProvenance::Estimated
                    } else {
                        UsageProvenance::ProviderReported
                    },
                }
            });
            state.current_context_tokens = usage
                .input_tokens
                .saturating_add(usage.cache_read_input_tokens)
                .saturating_add(usage.cache_creation_input_tokens);
            let _ = events.send(RuntimeEvent::Usage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_read_input_tokens: usage.cache_read_input_tokens,
                cache_creation_input_tokens: usage.cache_creation_input_tokens,
                total_tokens: usage.total_tokens,
                duration_ms: usage.duration_ms,
                tokens_per_second_milli: usage.tokens_per_second_milli,
                estimated_cost_microusd: usage.estimated_cost_microusd,
                provenance: usage.provenance,
            });
        }
'''
support = replace_once(support, old_branch, new_branch, "support usage branch")
support_path.write_text(support)

app_path = Path("crates/medusa-tui/src/app.rs")
app = app_path.read_text()
app = replace_once(
    app,
    "        self.tokens_per_second_milli = tokens_per_second_milli;\n        self.usage_provenance = Some(provenance);",
    "        let _ = tokens_per_second_milli;\n        self.usage_provenance = Some(provenance);",
    "app latest rate assignment",
)
app = replace_once(
    app,
    "        self.model_elapsed_millis = self.model_elapsed_millis.saturating_add(duration_ms);",
    "        self.model_elapsed_millis = self.model_elapsed_millis.saturating_add(duration_ms);\n        self.tokens_per_second_milli = if self.model_elapsed_millis == 0 {\n            0\n        } else {\n            self.total_tokens.saturating_mul(1_000_000) / self.model_elapsed_millis\n        };",
    "app cumulative rate",
)
app_path.write_text(app)

tests_path = Path("crates/medusa-runtime/src/tests.rs")
tests = tests_path.read_text()
pattern = re.compile(r'''#\[test\]\nfn provider_usage_forwards_input_output_cache_and_model_time\(\) \{.*?\n\}\n\n(?=#\[test\]\nfn runtime_events_preserve_agent_plan_contracts)''', re.S)
replacement = '''#[test]
fn provider_usage_forwards_legacy_and_normalized_telemetry() {
    let (sender, receiver) = mpsc::channel();
    let mut state = UpdateState::new();
    forward_update(
        &AgentUpdate::Event(EventPayload::ModelRequestStarted {
            provider: "minimax".to_owned(),
            model: "MiniMax-M3".to_owned(),
        }),
        &sender,
        &mut state,
    );
    forward_update(
        &AgentUpdate::Event(EventPayload::ModelResponseReceived {
            response_id: Some("legacy-response".to_owned()),
            usage: json!({
                "input_tokens": 120,
                "output_tokens": 30,
                "cache_read_input_tokens": 80,
                "cache_creation_input_tokens": 20
            }),
        }),
        &sender,
        &mut state,
    );
    assert!(matches!(
        receiver.recv().expect("legacy usage event"),
        RuntimeEvent::Usage {
            input_tokens: 120,
            output_tokens: 30,
            cache_read_input_tokens: 80,
            cache_creation_input_tokens: 20,
            total_tokens: 250,
            duration_ms,
            provenance: UsageProvenance::ProviderReported,
            ..
        } if duration_ms >= 1
    ));
    assert_eq!(state.current_context_tokens, 220);

    forward_update(
        &AgentUpdate::Event(EventPayload::ModelResponseReceived {
            response_id: Some("normalized-response".to_owned()),
            usage: json!({
                "turn": 2,
                "input_tokens": 10,
                "output_tokens": 5,
                "cache_read_input_tokens": 2,
                "cache_creation_input_tokens": 1,
                "total_tokens": 18,
                "duration_ms": 100,
                "tokens_per_second_milli": 180_000,
                "estimated_cost_microusd": 7,
                "provenance": "provider_reported"
            }),
        }),
        &sender,
        &mut state,
    );
    assert!(matches!(
        receiver.recv().expect("normalized usage event"),
        RuntimeEvent::Usage {
            total_tokens: 18,
            duration_ms: 100,
            tokens_per_second_milli: 180_000,
            estimated_cost_microusd: 7,
            provenance: UsageProvenance::ProviderReported,
            ..
        }
    ));
}

'''
tests, count = pattern.subn(replacement, tests, count=1)
if count != 1:
    raise SystemExit(f"runtime tests: expected one function replacement, found {count}")
tests_path.write_text(tests)
