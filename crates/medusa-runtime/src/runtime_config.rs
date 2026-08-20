//! Typed, versioned, fail-closed configuration for tunable runtime-loop behavior.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const RUNTIME_CONFIG_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeLoopConfigV1 {
    pub schema_version: u16,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub tool_presentation: ToolPresentationConfig,
    pub retry_budget: u32,
    pub replan_budget: u32,
    pub timeout_millis: u64,
    pub compaction_threshold_tokens: u64,
    pub model_output_chars: usize,
    pub service_provider: Option<String>,
    pub diagnostics_enabled: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPresentationConfig {
    Native,
    Code,
    Both,
}

impl Default for RuntimeLoopConfigV1 {
    fn default() -> Self {
        Self {
            schema_version: RUNTIME_CONFIG_SCHEMA_VERSION,
            provider: None,
            model: None,
            tool_presentation: ToolPresentationConfig::Native,
            retry_budget: 2,
            replan_budget: 2,
            timeout_millis: 120_000,
            compaction_threshold_tokens: 100_000,
            model_output_chars: 256 * 1024,
            service_provider: None,
            diagnostics_enabled: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeConfigHardLimits {
    pub max_retry_budget: u32,
    pub max_replan_budget: u32,
    pub max_timeout_millis: u64,
    pub max_compaction_threshold_tokens: u64,
    pub max_model_output_chars: usize,
}

impl Default for RuntimeConfigHardLimits {
    fn default() -> Self {
        Self {
            max_retry_budget: 8,
            max_replan_budget: 8,
            max_timeout_millis: 15 * 60 * 1_000,
            max_compaction_threshold_tokens: 1_000_000,
            max_model_output_chars: 2 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EffectiveRuntimeConfigV1 {
    pub schema_version: u16,
    pub config: RuntimeLoopConfigV1,
    pub provenance: BTreeMap<String, String>,
    pub frozen_for_session: bool,
    pub fingerprint: String,
}

pub fn compile_effective_config(
    config: RuntimeLoopConfigV1,
    provenance: BTreeMap<String, String>,
    limits: RuntimeConfigHardLimits,
    code_mode_ready: bool,
) -> Result<EffectiveRuntimeConfigV1, Vec<String>> {
    let mut errors = Vec::new();
    if config.schema_version != RUNTIME_CONFIG_SCHEMA_VERSION {
        errors.push("unsupported runtime configuration schema".to_owned());
    }
    if config.retry_budget > limits.max_retry_budget {
        errors.push("retry_budget exceeds the hard policy maximum".to_owned());
    }
    if config.replan_budget > limits.max_replan_budget {
        errors.push("replan_budget exceeds the hard policy maximum".to_owned());
    }
    if config.timeout_millis == 0 || config.timeout_millis > limits.max_timeout_millis {
        errors.push("timeout_millis is outside the hard policy range".to_owned());
    }
    if config.compaction_threshold_tokens == 0
        || config.compaction_threshold_tokens > limits.max_compaction_threshold_tokens
    {
        errors.push("compaction threshold is outside the hard policy range".to_owned());
    }
    if config.model_output_chars == 0 || config.model_output_chars > limits.max_model_output_chars {
        errors.push("model output bound is outside the hard policy range".to_owned());
    }
    if matches!(
        config.tool_presentation,
        ToolPresentationConfig::Code | ToolPresentationConfig::Both
    ) && !code_mode_ready
    {
        errors.push("Code Mode requires an available certified capability".to_owned());
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let canonical = serde_json::to_vec(&config).unwrap_or_default();
    let fingerprint = Sha256::digest(canonical)
        .iter()
        .map(|byte| format!("{:02x}", byte))
        .collect();
    Ok(EffectiveRuntimeConfigV1 {
        schema_version: RUNTIME_CONFIG_SCHEMA_VERSION,
        config,
        provenance,
        frozen_for_session: true,
        fingerprint,
    })
}

#[must_use]
pub fn explain_config(config: &EffectiveRuntimeConfigV1) -> Value {
    serde_json::json!({
        "schema_version": config.schema_version,
        "fingerprint": config.fingerprint,
        "frozen_for_session": config.frozen_for_session,
        "provenance": config.provenance,
        "config": config.config,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_code_mode_fails_before_side_effects() {
        let config = RuntimeLoopConfigV1 {
            tool_presentation: ToolPresentationConfig::Code,
            ..RuntimeLoopConfigV1::default()
        };
        assert!(
            compile_effective_config(
                config,
                BTreeMap::new(),
                RuntimeConfigHardLimits::default(),
                false
            )
            .is_err()
        );
    }

    #[test]
    fn effective_config_is_frozen_and_fingerprinted() {
        let effective = compile_effective_config(
            RuntimeLoopConfigV1::default(),
            BTreeMap::from([("retry_budget".to_owned(), "built_in_default".to_owned())]),
            RuntimeConfigHardLimits::default(),
            true,
        )
        .expect("effective config");
        assert!(effective.frozen_for_session);
        assert!(!effective.fingerprint.is_empty());
        assert_eq!(
            explain_config(&effective)["fingerprint"],
            effective.fingerprint
        );
    }
}
