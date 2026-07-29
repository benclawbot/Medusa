//! Deterministic, inspectable policy primitives for tool orchestration.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ToolEffect {
    ReadOnly,
    Mutating,
    Destructive,
    ExternallyVisible,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum OutputMode {
    Compact,
    Normal,
    Verbatim,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum VerificationLevel {
    None,
    Syntax,
    Focused,
    Package,
    Workspace,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolEstimate {
    pub latency_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub retained_tokens: u64,
    pub monetary_microunits: u64,
    pub success_probability: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolMetadata {
    pub name: String,
    pub intents: BTreeSet<String>,
    pub effect: ToolEffect,
    pub estimate: ToolEstimate,
    pub requires_network: bool,
    pub cancellable: bool,
    pub idempotent: bool,
    pub concurrency_group: Option<String>,
    pub max_output_bytes: u64,
    pub output_modes: BTreeSet<OutputMode>,
    pub fallback_tools: Vec<String>,
    pub cacheable: bool,
    pub verification: VerificationLevel,
}

impl ToolMetadata {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.name.trim().is_empty() || self.intents.is_empty() || self.output_modes.is_empty() {
            return Err("tool metadata requires name, intent, and output mode");
        }
        if !(0.0..=1.0).contains(&self.estimate.success_probability) {
            return Err("success probability must be between zero and one");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ToolBudgets {
    pub remaining_calls: u32,
    pub remaining_time_ms: u64,
    pub remaining_input_tokens: u64,
    pub remaining_output_tokens: u64,
    pub remaining_monetary_microunits: u64,
    pub reserved_verification_calls: u32,
    pub reserved_final_tokens: u64,
}

impl ToolBudgets {
    fn can_afford(&self, estimate: &ToolEstimate) -> bool {
        self.remaining_calls > self.reserved_verification_calls
            && self.remaining_time_ms >= estimate.latency_ms
            && self.remaining_input_tokens >= estimate.input_tokens
            && self.remaining_output_tokens
                >= estimate
                    .output_tokens
                    .saturating_add(self.reserved_final_tokens)
            && self.remaining_monetary_microunits >= estimate.monetary_microunits
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolRecommendation {
    pub tool: String,
    pub score: f64,
    pub rationale: Vec<String>,
    pub output_mode: OutputMode,
    pub alternatives: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ToolRegistry {
    tools: BTreeMap<String, ToolMetadata>,
}

impl ToolRegistry {
    pub fn register(&mut self, metadata: ToolMetadata) -> Result<(), &'static str> {
        metadata.validate()?;
        if self.tools.insert(metadata.name.clone(), metadata).is_some() {
            return Err("tool is already registered");
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn recommend(
        &self,
        intent: &str,
        budgets: &ToolBudgets,
        allow_network: bool,
        allow_mutation: bool,
        mode: OutputMode,
        cache_hits: &BTreeSet<String>,
    ) -> Vec<ToolRecommendation> {
        let mut ranked = self
            .tools
            .values()
            .filter(|tool| {
                tool.intents.contains(intent)
                    && budgets.can_afford(&tool.estimate)
                    && (allow_network || !tool.requires_network)
                    && (allow_mutation || tool.effect == ToolEffect::ReadOnly)
            })
            .map(|tool| {
                let cache_bonus = if cache_hits.contains(&tool.name) {
                    10.0
                } else {
                    0.0
                };
                let risk = if tool.effect == ToolEffect::ReadOnly {
                    0.0
                } else {
                    20.0
                };
                let score = tool.estimate.success_probability * 100.0
                    - tool.estimate.latency_ms as f64 * 0.001
                    - tool.estimate.output_tokens as f64 * 0.02
                    - risk
                    + cache_bonus;
                ToolRecommendation {
                    tool: tool.name.clone(),
                    score,
                    rationale: vec![
                        format!("supports {intent}"),
                        format!("estimated output {} tokens", tool.estimate.output_tokens),
                    ],
                    output_mode: if tool.output_modes.contains(&mode) {
                        mode
                    } else {
                        OutputMode::Normal
                    },
                    alternatives: Vec::new(),
                }
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.tool.cmp(&right.tool))
        });
        let names = ranked
            .iter()
            .map(|item| item.tool.clone())
            .collect::<Vec<_>>();
        for item in &mut ranked {
            item.alternatives = names
                .iter()
                .filter(|name| *name != &item.tool)
                .take(3)
                .cloned()
                .collect();
        }
        ranked
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OutputEnvelope {
    pub mode: OutputMode,
    pub rendered: String,
    pub original_lines: usize,
    pub omitted_lines: usize,
    pub expansion_handle: Option<String>,
}

pub fn compact_failures(
    input: &str,
    limit: usize,
    expansion_handle: impl Into<String>,
) -> OutputEnvelope {
    let lines = input.lines().collect::<Vec<_>>();
    let failures = lines
        .iter()
        .copied()
        .filter(|line| {
            let value = line.to_ascii_lowercase();
            value.contains("error")
                || value.contains("failed")
                || value.contains("panic")
                || value.contains("assert")
        })
        .collect::<Vec<_>>();
    let retained = if failures.is_empty() {
        lines.iter().take(limit).copied().collect()
    } else {
        failures
    };
    let omitted_lines = lines.len().saturating_sub(retained.len());
    OutputEnvelope {
        mode: OutputMode::Compact,
        rendered: retained.join("\n"),
        original_lines: lines.len(),
        omitted_lines,
        expansion_handle: (omitted_lines > 0).then(|| expansion_handle.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(name: &str, latency_ms: u64, output_tokens: u64) -> ToolMetadata {
        ToolMetadata {
            name: name.to_owned(),
            intents: BTreeSet::from(["search".to_owned()]),
            effect: ToolEffect::ReadOnly,
            estimate: ToolEstimate {
                latency_ms,
                input_tokens: 10,
                output_tokens,
                retained_tokens: output_tokens,
                monetary_microunits: 1,
                success_probability: 0.9,
            },
            requires_network: false,
            cancellable: true,
            idempotent: true,
            concurrency_group: None,
            max_output_bytes: 4096,
            output_modes: BTreeSet::from([OutputMode::Compact, OutputMode::Normal]),
            fallback_tools: Vec::new(),
            cacheable: true,
            verification: VerificationLevel::None,
        }
    }

    fn budgets() -> ToolBudgets {
        ToolBudgets {
            remaining_calls: 3,
            remaining_time_ms: 5_000,
            remaining_input_tokens: 1_000,
            remaining_output_tokens: 1_000,
            remaining_monetary_microunits: 100,
            reserved_verification_calls: 1,
            reserved_final_tokens: 100,
        }
    }

    #[test]
    fn ranking_prefers_narrower_output_and_cache_hits() {
        let mut registry = ToolRegistry::default();
        registry.register(metadata("broad", 50, 300)).unwrap();
        registry.register(metadata("narrow", 100, 20)).unwrap();
        let ranked = registry.recommend(
            "search",
            &budgets(),
            false,
            false,
            OutputMode::Compact,
            &BTreeSet::from(["narrow".to_owned()]),
        );
        assert_eq!(ranked[0].tool, "narrow");
        assert_eq!(ranked[0].alternatives, vec!["broad"]);
    }

    #[test]
    fn reserved_capacity_blocks_unaffordable_calls() {
        let mut registry = ToolRegistry::default();
        registry.register(metadata("search", 100, 950)).unwrap();
        assert!(
            registry
                .recommend(
                    "search",
                    &budgets(),
                    false,
                    false,
                    OutputMode::Compact,
                    &BTreeSet::new(),
                )
                .is_empty()
        );
    }

    #[test]
    fn compact_output_preserves_failures_and_expansion_handle() {
        let output = compact_failures("ok\nerror: broken\nok", 1, "run:1");
        assert_eq!(output.rendered, "error: broken");
        assert_eq!(output.omitted_lines, 2);
        assert_eq!(output.expansion_handle.as_deref(), Some("run:1"));
    }
}
