from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"expected one match in {path}, found {count}: {old[:100]!r}")
    file.write_text(text.replace(old, new, 1))


# Export the provenance type through medusa-runtime so the TUI does not depend on medusa-agent.
replace_once(
    "crates/medusa-runtime/src/lib.rs",
    "    AgentEngine, AgentPlanStep, AgentQuestion, AgentSession, StepOutcome, TurnUsage,\n    UsageProvenance, compact_session, update_session_objective,\n",
    "    AgentEngine, AgentPlanStep, AgentQuestion, AgentSession, StepOutcome, TurnUsage,\n    compact_session, update_session_objective,\n",
)
replace_once(
    "crates/medusa-runtime/src/lib.rs",
    "pub use medusa_agent::{\n    AgentPlanStep as RuntimePlanStep, AgentPlanStepStatus, AgentQuestionItem, AgentQuestionOption,\n};\n",
    "pub use medusa_agent::{\n    AgentPlanStep as RuntimePlanStep, AgentPlanStepStatus, AgentQuestionItem, AgentQuestionOption,\n    UsageProvenance,\n};\n",
)
replace_once(
    "crates/medusa-tui/src/runtime.rs",
    "                medusa_agent::UsageProvenance::ProviderReported => \"provider\".to_owned(),\n                medusa_agent::UsageProvenance::Estimated => \"estimated\".to_owned(),\n",
    "                medusa_runtime::UsageProvenance::ProviderReported => \"provider\".to_owned(),\n                medusa_runtime::UsageProvenance::Estimated => \"estimated\".to_owned(),\n",
)

# Keep and display the normalized per-turn throughput supplied by TurnUsage.
replace_once(
    "crates/medusa-tui/src/app.rs",
    "    pub estimated_cost_microusd: u64,\n    pub usage_provenance: Option<String>,\n",
    "    pub estimated_cost_microusd: u64,\n    pub tokens_per_second_milli: u64,\n    pub usage_provenance: Option<String>,\n",
)
replace_once(
    "crates/medusa-tui/src/app.rs",
    "            estimated_cost_microusd: 0,\n            usage_provenance: None,\n",
    "            estimated_cost_microusd: 0,\n            tokens_per_second_milli: 0,\n            usage_provenance: None,\n",
)
replace_once(
    "crates/medusa-tui/src/app.rs",
    "        self.estimated_cost_microusd = 0;\n        self.usage_provenance = None;\n",
    "        self.estimated_cost_microusd = 0;\n        self.tokens_per_second_milli = 0;\n        self.usage_provenance = None;\n",
)
replace_once(
    "crates/medusa-tui/src/app.rs",
    "            model_elapsed_millis,\n            0,\n            0,\n            \"estimated\".to_owned(),\n",
    "            model_elapsed_millis,\n            if model_elapsed_millis == 0 {\n                0\n            } else {\n                output_tokens.saturating_mul(1_000_000) / model_elapsed_millis\n            },\n            0,\n            \"estimated\".to_owned(),\n",
)
replace_once(
    "crates/medusa-tui/src/app.rs",
    "        _tokens_per_second_milli: u64,\n",
    "        tokens_per_second_milli: u64,\n",
)
replace_once(
    "crates/medusa-tui/src/app.rs",
    "        self.estimated_cost_microusd = self\n            .estimated_cost_microusd\n            .saturating_add(estimated_cost_microusd);\n        self.usage_provenance = Some(provenance);\n",
    "        self.estimated_cost_microusd = self\n            .estimated_cost_microusd\n            .saturating_add(estimated_cost_microusd);\n        self.tokens_per_second_milli = tokens_per_second_milli;\n        self.usage_provenance = Some(provenance);\n",
)
replace_once(
    "crates/medusa-tui/src/app.rs",
    "        (self.model_elapsed_millis > 0)\n            .then(|| self.total_tokens as f64 * 1_000.0 / self.model_elapsed_millis as f64)\n",
    "        (self.tokens_per_second_milli > 0)\n            .then(|| self.tokens_per_second_milli as f64 / 1_000.0)\n",
)

# Visible render snapshots must notice cost/provenance/rate-only changes.
replace_once(
    "crates/medusa-tui/src/render.rs",
    "    timed_output_tokens: u64,\n    cache_read_input_tokens: u64,\n",
    "    timed_output_tokens: u64,\n    total_tokens: u64,\n    estimated_cost_microusd: u64,\n    tokens_per_second_milli: u64,\n    usage_provenance: Option<String>,\n    cache_read_input_tokens: u64,\n",
)
replace_once(
    "crates/medusa-tui/src/render.rs",
    "        timed_output_tokens: app.timed_output_tokens,\n        cache_read_input_tokens: app.cache_read_input_tokens,\n",
    "        timed_output_tokens: app.timed_output_tokens,\n        total_tokens: app.total_tokens,\n        estimated_cost_microusd: app.estimated_cost_microusd,\n        tokens_per_second_milli: app.tokens_per_second_milli,\n        usage_provenance: app.usage_provenance.clone(),\n        cache_read_input_tokens: app.cache_read_input_tokens,\n",
)

replace_once(
    "crates/medusa-tui/src/lib.rs",
    "            \"session 0s · total 2.3k · input 700 · output 1.5k · cache-read 200 · cache-write 100 · cost — · estimated · 900.0 tok/s\"\n",
    "            \"session 0s · total 2.5k · input 700 · output 1.5k · cache-read 200 · cache-write 100 · cost — · estimated · 600.0 tok/s\"\n",
)

# Remove accidental extra spacing before the appended README section.
replace_once(
    "README.md",
    "\n\n\n\n## Usage telemetry\n",
    "\n\n## Usage telemetry\n",
)

Path("scripts/apply-frontend-telemetry-fix.py").unlink()
Path(".github/workflows/apply-frontend-telemetry-fix.yml").unlink()
