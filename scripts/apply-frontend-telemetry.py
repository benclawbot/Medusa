from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"expected one match in {path}, found {count}: {old[:80]!r}")
    file.write_text(text.replace(old, new, 1))


# Runtime: carry the normalized durable TurnUsage rather than reconstructing usage locally.
replace_once(
    "crates/medusa-runtime/src/lib.rs",
    "    AgentEngine, AgentPlanStep, AgentQuestion, AgentSession, StepOutcome, compact_session,\n    update_session_objective,\n",
    "    AgentEngine, AgentPlanStep, AgentQuestion, AgentSession, StepOutcome, TurnUsage,\n    UsageProvenance, compact_session, update_session_objective,\n",
)
replace_once(
    "crates/medusa-runtime/src/lib.rs",
    "    Usage {\n        input_tokens: u64,\n        output_tokens: u64,\n        cache_read_input_tokens: u64,\n        cache_creation_input_tokens: u64,\n        model_elapsed_millis: u64,\n    },\n",
    "    Usage {\n        input_tokens: u64,\n        output_tokens: u64,\n        cache_read_input_tokens: u64,\n        cache_creation_input_tokens: u64,\n        total_tokens: u64,\n        duration_ms: u64,\n        tokens_per_second_milli: u64,\n        estimated_cost_microusd: u64,\n        provenance: UsageProvenance,\n    },\n",
)

replace_once(
    "crates/medusa-runtime/src/support.rs",
    "    sync::mpsc::Sender,\n    time::Instant,\n",
    "    sync::mpsc::Sender,\n",
)
replace_once(
    "crates/medusa-runtime/src/support.rs",
    "    model_started_at: Option<Instant>,\n",
    "",
)
replace_once(
    "crates/medusa-runtime/src/support.rs",
    "            model_started_at: None,\n",
    "",
)
replace_once(
    "crates/medusa-runtime/src/support.rs",
    "        AgentUpdate::Event(EventPayload::ModelRequestStarted { .. }) => {\n            state.model_started_at = Some(Instant::now());\n        }\n        AgentUpdate::Event(EventPayload::ModelResponseReceived { usage, .. }) => {\n            let model_elapsed_millis = state.model_started_at.take().map_or(0, |started_at| {\n                u64::try_from(started_at.elapsed().as_millis())\n                    .unwrap_or(u64::MAX)\n                    .max(1)\n            });\n            let input_tokens = usage\n                .get(\"input_tokens\")\n                .and_then(Value::as_u64)\n                .unwrap_or_default();\n            let output_tokens = usage\n                .get(\"output_tokens\")\n                .and_then(Value::as_u64)\n                .unwrap_or_default();\n            let cache_read_input_tokens = usage\n                .get(\"cache_read_input_tokens\")\n                .and_then(Value::as_u64)\n                .unwrap_or_default();\n            let cache_creation_input_tokens = usage\n                .get(\"cache_creation_input_tokens\")\n                .and_then(Value::as_u64)\n                .unwrap_or_default();\n            state.current_context_tokens = input_tokens\n                .saturating_add(cache_read_input_tokens)\n                .saturating_add(cache_creation_input_tokens);\n            let _ = events.send(RuntimeEvent::Usage {\n                input_tokens,\n                output_tokens,\n                cache_read_input_tokens,\n                cache_creation_input_tokens,\n                model_elapsed_millis,\n            });\n        }\n",
    "        AgentUpdate::Event(EventPayload::ModelResponseReceived { usage, .. }) => {\n            let Ok(usage) = serde_json::from_value::<TurnUsage>(usage.clone()) else {\n                return;\n            };\n            state.current_context_tokens = usage\n                .input_tokens\n                .saturating_add(usage.cache_read_input_tokens)\n                .saturating_add(usage.cache_creation_input_tokens);\n            let _ = events.send(RuntimeEvent::Usage {\n                input_tokens: usage.input_tokens,\n                output_tokens: usage.output_tokens,\n                cache_read_input_tokens: usage.cache_read_input_tokens,\n                cache_creation_input_tokens: usage.cache_creation_input_tokens,\n                total_tokens: usage.total_tokens,\n                duration_ms: usage.duration_ms,\n                tokens_per_second_milli: usage.tokens_per_second_milli,\n                estimated_cost_microusd: usage.estimated_cost_microusd,\n                provenance: usage.provenance,\n            });\n        }\n",
)

# TUI runtime adapter: preserve every normalized telemetry field.
replace_once(
    "crates/medusa-tui/src/runtime.rs",
    "        model_elapsed_millis: u64,\n",
    "        total_tokens: u64,\n        duration_ms: u64,\n        tokens_per_second_milli: u64,\n        estimated_cost_microusd: u64,\n        provenance: String,\n",
)
replace_once(
    "crates/medusa-tui/src/runtime.rs",
    "            model_elapsed_millis,\n        } => RuntimeEvent::Usage {\n            input_tokens,\n            output_tokens,\n            cache_read_input_tokens,\n            cache_creation_input_tokens,\n            model_elapsed_millis,\n        },\n",
    "            total_tokens,\n            duration_ms,\n            tokens_per_second_milli,\n            estimated_cost_microusd,\n            provenance,\n        } => RuntimeEvent::Usage {\n            input_tokens,\n            output_tokens,\n            cache_read_input_tokens,\n            cache_creation_input_tokens,\n            total_tokens,\n            duration_ms,\n            tokens_per_second_milli,\n            estimated_cost_microusd,\n            provenance: match provenance {\n                medusa_agent::UsageProvenance::ProviderReported => \"provider\".to_owned(),\n                medusa_agent::UsageProvenance::Estimated => \"estimated\".to_owned(),\n            },\n        },\n",
)

# TUI state: retain authoritative totals, cost, and provenance.
replace_once(
    "crates/medusa-tui/src/app.rs",
    "    pub timed_output_tokens: u64,\n    pub cache_read_input_tokens: u64,\n",
    "    pub timed_output_tokens: u64,\n    pub total_tokens: u64,\n    pub estimated_cost_microusd: u64,\n    pub usage_provenance: Option<String>,\n    pub cache_read_input_tokens: u64,\n",
)
replace_once(
    "crates/medusa-tui/src/app.rs",
    "            timed_output_tokens: 0,\n            cache_read_input_tokens: 0,\n",
    "            timed_output_tokens: 0,\n            total_tokens: 0,\n            estimated_cost_microusd: 0,\n            usage_provenance: None,\n            cache_read_input_tokens: 0,\n",
)
replace_once(
    "crates/medusa-tui/src/app.rs",
    "        self.timed_output_tokens = 0;\n        self.cache_read_input_tokens = 0;\n",
    "        self.timed_output_tokens = 0;\n        self.total_tokens = 0;\n        self.estimated_cost_microusd = 0;\n        self.usage_provenance = None;\n        self.cache_read_input_tokens = 0;\n",
)
replace_once(
    "crates/medusa-tui/src/app.rs",
    "    pub fn record_usage(\n        &mut self,\n        input_tokens: u64,\n        output_tokens: u64,\n        cache_read_input_tokens: u64,\n        cache_creation_input_tokens: u64,\n        model_elapsed_millis: u64,\n    ) {\n        self.input_tokens = self.input_tokens.saturating_add(input_tokens);\n        self.output_tokens = self.output_tokens.saturating_add(output_tokens);\n        if model_elapsed_millis > 0 {\n            self.timed_output_tokens = self.timed_output_tokens.saturating_add(output_tokens);\n        }\n        self.cache_read_input_tokens = self\n            .cache_read_input_tokens\n            .saturating_add(cache_read_input_tokens);\n        self.cache_creation_input_tokens = self\n            .cache_creation_input_tokens\n            .saturating_add(cache_creation_input_tokens);\n        self.current_context_tokens = input_tokens\n            .saturating_add(cache_read_input_tokens)\n            .saturating_add(cache_creation_input_tokens);\n        self.model_elapsed_millis = self\n            .model_elapsed_millis\n            .saturating_add(model_elapsed_millis);\n    }\n",
    "    pub fn record_usage(\n        &mut self,\n        input_tokens: u64,\n        output_tokens: u64,\n        cache_read_input_tokens: u64,\n        cache_creation_input_tokens: u64,\n        model_elapsed_millis: u64,\n    ) {\n        let total_tokens = input_tokens\n            .saturating_add(output_tokens)\n            .saturating_add(cache_read_input_tokens)\n            .saturating_add(cache_creation_input_tokens);\n        self.record_turn_usage(\n            input_tokens,\n            output_tokens,\n            cache_read_input_tokens,\n            cache_creation_input_tokens,\n            total_tokens,\n            model_elapsed_millis,\n            0,\n            0,\n            \"estimated\".to_owned(),\n        );\n    }\n\n    #[allow(clippy::too_many_arguments)]\n    pub fn record_turn_usage(\n        &mut self,\n        input_tokens: u64,\n        output_tokens: u64,\n        cache_read_input_tokens: u64,\n        cache_creation_input_tokens: u64,\n        total_tokens: u64,\n        duration_ms: u64,\n        _tokens_per_second_milli: u64,\n        estimated_cost_microusd: u64,\n        provenance: String,\n    ) {\n        self.input_tokens = self.input_tokens.saturating_add(input_tokens);\n        self.output_tokens = self.output_tokens.saturating_add(output_tokens);\n        self.timed_output_tokens = self.timed_output_tokens.saturating_add(output_tokens);\n        self.total_tokens = self.total_tokens.saturating_add(total_tokens);\n        self.estimated_cost_microusd = self\n            .estimated_cost_microusd\n            .saturating_add(estimated_cost_microusd);\n        self.usage_provenance = Some(provenance);\n        self.cache_read_input_tokens = self\n            .cache_read_input_tokens\n            .saturating_add(cache_read_input_tokens);\n        self.cache_creation_input_tokens = self\n            .cache_creation_input_tokens\n            .saturating_add(cache_creation_input_tokens);\n        self.current_context_tokens = input_tokens\n            .saturating_add(cache_read_input_tokens)\n            .saturating_add(cache_creation_input_tokens);\n        self.model_elapsed_millis = self.model_elapsed_millis.saturating_add(duration_ms);\n    }\n",
)
replace_once(
    "crates/medusa-tui/src/app.rs",
    "        (self.model_elapsed_millis > 0)\n            .then(|| self.timed_output_tokens as f64 * 1_000.0 / self.model_elapsed_millis as f64)\n",
    "        (self.model_elapsed_millis > 0)\n            .then(|| self.total_tokens as f64 * 1_000.0 / self.model_elapsed_millis as f64)\n",
)

# Event drain: use the expanded authoritative telemetry payload.
replace_once(
    "crates/medusa-tui/src/session.rs",
    "                model_elapsed_millis,\n            } => {\n                app.record_usage(\n                    input_tokens,\n                    output_tokens,\n                    cache_read_input_tokens,\n                    cache_creation_input_tokens,\n                    model_elapsed_millis,\n                );\n            }\n",
    "                total_tokens,\n                duration_ms,\n                tokens_per_second_milli,\n                estimated_cost_microusd,\n                provenance,\n            } => {\n                app.record_turn_usage(\n                    input_tokens,\n                    output_tokens,\n                    cache_read_input_tokens,\n                    cache_creation_input_tokens,\n                    total_tokens,\n                    duration_ms,\n                    tokens_per_second_milli,\n                    estimated_cost_microusd,\n                    provenance,\n                );\n            }\n",
)

# Header rendering: expose total tokens, estimated cost, and source provenance.
replace_once(
    "crates/medusa-tui/src/render.rs",
    "        \"session {} · input {} · output {} · cache-read {} · cache-write {} · {rate} tok/s\",\n        format_elapsed(app.session_elapsed_seconds()),\n        format_token_count(app.input_tokens),\n        format_token_count(app.output_tokens),\n        format_token_count(app.cache_read_input_tokens),\n        format_token_count(app.cache_creation_input_tokens),\n",
    "        \"session {} · total {} · input {} · output {} · cache-read {} · cache-write {} · cost {} · {} · {rate} tok/s\",\n        format_elapsed(app.session_elapsed_seconds()),\n        format_token_count(app.total_tokens),\n        format_token_count(app.input_tokens),\n        format_token_count(app.output_tokens),\n        format_token_count(app.cache_read_input_tokens),\n        format_token_count(app.cache_creation_input_tokens),\n        format_cost(app.estimated_cost_microusd),\n        app.usage_provenance.as_deref().unwrap_or(\"—\"),\n",
)
replace_once(
    "crates/medusa-tui/src/render.rs",
    "fn format_token_rate(tokens_per_second: f64) -> String {\n",
    "fn format_cost(microusd: u64) -> String {\n    if microusd == 0 {\n        return \"—\".to_owned();\n    }\n    format!(\"${:.4}\", microusd as f64 / 1_000_000.0)\n}\n\nfn format_token_rate(tokens_per_second: f64) -> String {\n",
)
replace_once(
    "crates/medusa-tui/src/lib.rs",
    "            \"session 0s · input 700 · output 1.5k · cache-read 200 · cache-write 100 · 600.0 tok/s\"\n",
    "            \"session 0s · total 2.3k · input 700 · output 1.5k · cache-read 200 · cache-write 100 · cost — · estimated · 900.0 tok/s\"\n",
)

# Document the user-visible telemetry contract.
readme = Path("README.md")
text = readme.read_text()
marker = "## Usage telemetry\n"
if marker not in text:
    text += "\n\n## Usage telemetry\n\nMedusa records normalized per-turn usage in the durable session event stream and surfaces cumulative totals in the TUI header. The display includes input, output, cache-read, cache-write, total tokens, measured throughput, estimated cost, and whether counts were provider-reported or deterministically estimated. Cost rates are configured with `MEDUSA_INPUT_COST_MICROUSD_PER_MILLION`, `MEDUSA_OUTPUT_COST_MICROUSD_PER_MILLION`, `MEDUSA_CACHE_READ_COST_MICROUSD_PER_MILLION`, and `MEDUSA_CACHE_WRITE_COST_MICROUSD_PER_MILLION`.\n"
    readme.write_text(text)

# Remove the one-shot implementation machinery from the resulting source commit.
Path("scripts/apply-frontend-telemetry.py").unlink()
Path(".github/workflows/apply-frontend-telemetry.yml").unlink()
