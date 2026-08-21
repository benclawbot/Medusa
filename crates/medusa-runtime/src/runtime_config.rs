//! Typed, versioned, fail-closed configuration for tunable runtime-loop behavior.

use std::{collections::BTreeMap, fs, path::Path};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::service_provider::ServiceProviderRegistry;

pub const RUNTIME_CONFIG_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
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

impl RuntimeLoopConfigV1 {
    /// Parses a complete or partial runtime configuration document.
    ///
    /// Missing fields use the explicit schema defaults. Unknown fields are rejected so a
    /// configuration cannot advertise behavior that this runtime does not own.
    pub fn from_toml(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|error| error.to_string())
    }
}

/// A typed partial layer used to resolve user, repository, learned, and session policy.
///
/// Every field is still part of the closed `RuntimeLoopConfigV1` schema; `Option` only means
/// that the layer did not override that field. This keeps precedence deterministic without
/// falling back to stringly-typed maps.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeLoopConfigPatchV1 {
    pub schema_version: Option<u16>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub tool_presentation: Option<ToolPresentationConfig>,
    pub retry_budget: Option<u32>,
    pub replan_budget: Option<u32>,
    pub timeout_millis: Option<u64>,
    pub compaction_threshold_tokens: Option<u64>,
    pub model_output_chars: Option<usize>,
    pub service_provider: Option<String>,
    pub diagnostics_enabled: Option<bool>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum RuntimeConfigSource {
    BuiltInDefault,
    UserProfile,
    RepositoryPolicy,
    LearnedPolicy,
    SessionOverride,
}

impl RuntimeConfigSource {
    const fn label(self) -> &'static str {
        match self {
            Self::BuiltInDefault => "built_in_default",
            Self::UserProfile => "user_profile",
            Self::RepositoryPolicy => "repository_policy",
            Self::LearnedPolicy => "learned_policy",
            Self::SessionOverride => "session_override",
        }
    }
}

fn read_patch(path: &Path) -> Result<RuntimeLoopConfigPatchV1, String> {
    let text = fs::read_to_string(path).map_err(|error| {
        format!(
            "failed to read runtime configuration {}: {error}",
            path.display()
        )
    })?;
    toml::from_str(&text).map_err(|error| {
        format!(
            "failed to parse runtime configuration {}: {error}",
            path.display()
        )
    })
}

fn validate_patch(patch: &RuntimeLoopConfigPatchV1) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    if patch
        .schema_version
        .is_some_and(|version| version != RUNTIME_CONFIG_SCHEMA_VERSION)
    {
        errors.push("unsupported runtime configuration schema".to_owned());
    }
    if patch.provider.is_some() != patch.model.is_some() {
        errors.push("provider and model selections must be supplied together".to_owned());
    }
    if patch
        .provider
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
        || patch
            .model
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
    {
        errors.push("provider and model selections must not be empty".to_owned());
    }
    if patch
        .service_provider
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        errors.push("service_provider must not be empty when selected".to_owned());
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn apply_patch(
    config: &mut RuntimeLoopConfigV1,
    provenance: &mut BTreeMap<String, String>,
    patch: RuntimeLoopConfigPatchV1,
    source: RuntimeConfigSource,
) {
    let label = source.label().to_owned();
    macro_rules! apply {
        ($field:ident) => {
            if let Some(value) = patch.$field {
                config.$field = value;
                provenance.insert(stringify!($field).to_owned(), label.clone());
            }
        };
    }
    macro_rules! apply_optional {
        ($field:ident) => {
            if let Some(value) = patch.$field {
                config.$field = Some(value);
                provenance.insert(stringify!($field).to_owned(), label.clone());
            }
        };
    }
    apply!(schema_version);
    apply_optional!(provider);
    apply_optional!(model);
    apply!(tool_presentation);
    apply!(retry_budget);
    apply!(replan_budget);
    apply!(timeout_millis);
    apply!(compaction_threshold_tokens);
    apply!(model_output_chars);
    apply_optional!(service_provider);
    apply!(diagnostics_enabled);
}

fn file_patch(path: Option<&Path>, errors: &mut Vec<String>) -> Option<RuntimeLoopConfigPatchV1> {
    let path = path.filter(|path| path.exists())?;
    match read_patch(path) {
        Ok(patch) => {
            if let Err(mut patch_errors) = validate_patch(&patch) {
                errors.append(&mut patch_errors);
                None
            } else {
                Some(patch)
            }
        }
        Err(error) => {
            errors.push(error);
            None
        }
    }
}

/// Resolves the effective runtime-loop configuration in fixed precedence order.
///
/// Precedence is built-in defaults, user profile, repository policy, learned bounded policy,
/// then the active session override. Missing files are intentionally ignored; malformed files,
/// unknown fields, and invalid combinations fail closed before a provider or tool is created.
pub fn compile_layered_config(
    mut base: RuntimeLoopConfigV1,
    user: Option<&Path>,
    repository: Option<&Path>,
    learned: Option<RuntimeLoopConfigPatchV1>,
    session: Option<RuntimeLoopConfigPatchV1>,
    limits: RuntimeConfigHardLimits,
    code_mode_ready: bool,
) -> Result<EffectiveRuntimeConfigV1, Vec<String>> {
    let mut provenance = BTreeMap::new();
    provenance.insert(
        "schema_version".to_owned(),
        RuntimeConfigSource::BuiltInDefault.label().to_owned(),
    );
    provenance.insert(
        "tool_presentation".to_owned(),
        RuntimeConfigSource::BuiltInDefault.label().to_owned(),
    );
    provenance.insert(
        "retry_budget".to_owned(),
        RuntimeConfigSource::BuiltInDefault.label().to_owned(),
    );
    provenance.insert(
        "replan_budget".to_owned(),
        RuntimeConfigSource::BuiltInDefault.label().to_owned(),
    );
    provenance.insert(
        "timeout_millis".to_owned(),
        RuntimeConfigSource::BuiltInDefault.label().to_owned(),
    );
    provenance.insert(
        "compaction_threshold_tokens".to_owned(),
        RuntimeConfigSource::BuiltInDefault.label().to_owned(),
    );
    provenance.insert(
        "model_output_chars".to_owned(),
        RuntimeConfigSource::BuiltInDefault.label().to_owned(),
    );
    provenance.insert(
        "diagnostics_enabled".to_owned(),
        RuntimeConfigSource::BuiltInDefault.label().to_owned(),
    );
    if base.provider.is_some() {
        provenance.insert("provider".to_owned(), "resolved_model_config".to_owned());
    }
    if base.model.is_some() {
        provenance.insert("model".to_owned(), "resolved_model_config".to_owned());
    }
    if base.service_provider.is_some() {
        provenance.insert(
            "service_provider".to_owned(),
            "resolved_service_config".to_owned(),
        );
    }

    let mut errors = Vec::new();
    if let Some(patch) = file_patch(user, &mut errors) {
        apply_patch(
            &mut base,
            &mut provenance,
            patch,
            RuntimeConfigSource::UserProfile,
        );
    }
    if let Some(patch) = file_patch(repository, &mut errors) {
        apply_patch(
            &mut base,
            &mut provenance,
            patch,
            RuntimeConfigSource::RepositoryPolicy,
        );
    }
    if let Some(patch) = learned {
        if let Err(mut patch_errors) = validate_patch(&patch) {
            errors.append(&mut patch_errors);
        } else {
            apply_patch(
                &mut base,
                &mut provenance,
                patch,
                RuntimeConfigSource::LearnedPolicy,
            );
        }
    }
    if let Some(patch) = session {
        if let Err(mut patch_errors) = validate_patch(&patch) {
            errors.append(&mut patch_errors);
        } else {
            apply_patch(
                &mut base,
                &mut provenance,
                patch,
                RuntimeConfigSource::SessionOverride,
            );
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    compile_effective_config(base, provenance, limits, code_mode_ready)
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

#[derive(Serialize)]
struct ExecutionFingerprintMaterial<'a> {
    schema_version: u16,
    provider: &'a Option<String>,
    model: &'a Option<String>,
    tool_presentation: ToolPresentationConfig,
    retry_budget: u32,
    replan_budget: u32,
    timeout_millis: u64,
    compaction_threshold_tokens: u64,
    model_output_chars: usize,
    service_provider: &'a Option<String>,
}

pub fn compile_effective_config(
    config: RuntimeLoopConfigV1,
    provenance: BTreeMap<String, String>,
    limits: RuntimeConfigHardLimits,
    code_mode_ready: bool,
) -> Result<EffectiveRuntimeConfigV1, Vec<String>> {
    compile_effective_config_with_registry(config, provenance, limits, code_mode_ready, None)
}

/// Compiles an effective configuration against an admitted non-authority service registry.
///
/// The legacy entry point intentionally has no registry and therefore rejects every selected
/// service provider. Callers that own a registry must pass it explicitly; a string in config is
/// never enough to create or replace a runtime service.
pub fn compile_effective_config_with_registry(
    config: RuntimeLoopConfigV1,
    provenance: BTreeMap<String, String>,
    limits: RuntimeConfigHardLimits,
    code_mode_ready: bool,
    registry: Option<&ServiceProviderRegistry>,
) -> Result<EffectiveRuntimeConfigV1, Vec<String>> {
    let mut errors = Vec::new();
    if config.schema_version != RUNTIME_CONFIG_SCHEMA_VERSION {
        errors.push("unsupported runtime configuration schema".to_owned());
    }
    match (&config.provider, &config.model) {
        (Some(provider), Some(model)) if provider.trim().is_empty() || model.trim().is_empty() => {
            errors.push("provider and model selections must not be empty".to_owned());
        }
        (Some(_), None) | (None, Some(_)) => {
            errors.push("provider and model selections must be supplied together".to_owned());
        }
        _ => {}
    }
    if config
        .service_provider
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        errors.push("service_provider must not be empty when selected".to_owned());
    } else if let Some(provider_id) = config.service_provider.as_deref() {
        let registered = registry
            .map(|registry| registry.contains_provider(provider_id))
            .transpose()
            .map_err(|error| vec![error.to_string()])?
            .unwrap_or(false);
        if !registered {
            errors.push(
                "no certified non-authority service provider is registered for this runtime"
                    .to_owned(),
            );
        }
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
    let budget_envelope_millis = u64::from(config.retry_budget)
        .saturating_add(u64::from(config.replan_budget))
        .saturating_add(1)
        .saturating_mul(1_000);
    if config.timeout_millis < budget_envelope_millis {
        errors.push("timeout_millis is smaller than the retry/replan budget envelope".to_owned());
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
    let canonical = serde_json::to_vec(&ExecutionFingerprintMaterial {
        schema_version: config.schema_version,
        provider: &config.provider,
        model: &config.model,
        tool_presentation: config.tool_presentation,
        retry_budget: config.retry_budget,
        replan_budget: config.replan_budget,
        timeout_millis: config.timeout_millis,
        compaction_threshold_tokens: config.compaction_threshold_tokens,
        model_output_chars: config.model_output_chars,
        service_provider: &config.service_provider,
    })
    .unwrap_or_default();
    let fingerprint = Sha256::digest(canonical)
        .iter()
        .map(|byte| format!("{byte:02x}"))
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
        "fingerprint_inputs": [
            "schema_version",
            "provider",
            "model",
            "tool_presentation",
            "retry_budget",
            "replan_budget",
            "timeout_millis",
            "compaction_threshold_tokens",
            "model_output_chars",
            "service_provider",
        ],
        "non_fingerprint_inputs": ["diagnostics_enabled"],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    #[test]
    fn diagnostics_only_changes_do_not_perturb_execution_fingerprint() {
        let baseline = compile_effective_config(
            RuntimeLoopConfigV1::default(),
            BTreeMap::new(),
            RuntimeConfigHardLimits::default(),
            true,
        )
        .expect("baseline config");
        let diagnostic_config = RuntimeLoopConfigV1 {
            diagnostics_enabled: true,
            ..RuntimeLoopConfigV1::default()
        };
        let diagnostic = compile_effective_config(
            diagnostic_config,
            BTreeMap::new(),
            RuntimeConfigHardLimits::default(),
            true,
        )
        .expect("diagnostic config");

        assert_eq!(
            baseline.fingerprint, diagnostic.fingerprint,
            "observability-only settings must not change the execution plan"
        );
    }

    #[test]
    fn provider_and_model_selection_must_be_complete() {
        let missing_model = RuntimeLoopConfigV1 {
            provider: Some("openai".to_owned()),
            ..RuntimeLoopConfigV1::default()
        };
        assert!(
            compile_effective_config(
                missing_model,
                BTreeMap::new(),
                RuntimeConfigHardLimits::default(),
                true,
            )
            .is_err()
        );

        let missing_provider = RuntimeLoopConfigV1 {
            model: Some("gpt-test".to_owned()),
            ..RuntimeLoopConfigV1::default()
        };
        assert!(
            compile_effective_config(
                missing_provider,
                BTreeMap::new(),
                RuntimeConfigHardLimits::default(),
                true,
            )
            .is_err()
        );
    }

    #[test]
    fn whitespace_service_provider_is_rejected() {
        let config = RuntimeLoopConfigV1 {
            service_provider: Some("   ".to_owned()),
            ..RuntimeLoopConfigV1::default()
        };
        assert!(
            compile_effective_config(
                config,
                BTreeMap::new(),
                RuntimeConfigHardLimits::default(),
                true,
            )
            .is_err()
        );
    }

    #[test]
    fn unregistered_service_provider_is_rejected_before_execution() {
        let config = RuntimeLoopConfigV1 {
            service_provider: Some("unregistered-service".to_owned()),
            ..RuntimeLoopConfigV1::default()
        };
        let errors = compile_effective_config(
            config,
            BTreeMap::new(),
            RuntimeConfigHardLimits::default(),
            true,
        )
        .expect_err("unregistered service providers must fail closed");
        assert!(errors.iter().any(|error| {
            error.contains("no certified non-authority service provider is registered")
        }));
    }

    #[test]
    fn unknown_runtime_configuration_fields_fail_closed() {
        let parsed = serde_json::from_value::<RuntimeLoopConfigV1>(json!({
            "schema_version": RUNTIME_CONFIG_SCHEMA_VERSION,
            "provider": null,
            "model": null,
            "tool_presentation": "native",
            "retry_budget": 2,
            "replan_budget": 2,
            "timeout_millis": 120000,
            "compaction_threshold_tokens": 100000,
            "model_output_chars": 262144,
            "service_provider": null,
            "diagnostics_enabled": false,
            "unexpected": true
        }));
        assert!(parsed.is_err());
    }

    #[test]
    fn layered_configuration_is_deterministic_and_preserves_provenance() {
        let directory = tempfile::tempdir().expect("tempdir");
        let user = directory.path().join("user-runtime.toml");
        let repository = directory.path().join("repository-runtime.toml");
        std::fs::write(
            &user,
            "schema_version = 1\nretry_budget = 3\ntimeout_millis = 90_000\n",
        )
        .expect("user runtime config");
        std::fs::write(
            &repository,
            "schema_version = 1\nretry_budget = 4\ndiagnostics_enabled = true\n",
        )
        .expect("repository runtime config");
        let learned = RuntimeLoopConfigPatchV1 {
            replan_budget: Some(5),
            ..RuntimeLoopConfigPatchV1::default()
        };
        let session = RuntimeLoopConfigPatchV1 {
            retry_budget: Some(6),
            ..RuntimeLoopConfigPatchV1::default()
        };

        let first = compile_layered_config(
            RuntimeLoopConfigV1::default(),
            Some(&user),
            Some(&repository),
            Some(learned.clone()),
            Some(session.clone()),
            RuntimeConfigHardLimits::default(),
            true,
        )
        .expect("layered config");
        let second = compile_layered_config(
            RuntimeLoopConfigV1::default(),
            Some(&user),
            Some(&repository),
            Some(learned),
            Some(session),
            RuntimeConfigHardLimits::default(),
            true,
        )
        .expect("layered config");

        assert_eq!(first.fingerprint, second.fingerprint);
        assert_eq!(first.config.retry_budget, 6);
        assert_eq!(first.config.replan_budget, 5);
        assert_eq!(first.config.timeout_millis, 90_000);
        assert!(first.config.diagnostics_enabled);
        assert_eq!(
            first.provenance.get("retry_budget"),
            Some(&"session_override".to_owned())
        );
        assert_eq!(
            first.provenance.get("replan_budget"),
            Some(&"learned_policy".to_owned())
        );
        assert_eq!(
            first.provenance.get("diagnostics_enabled"),
            Some(&"repository_policy".to_owned())
        );
    }

    #[test]
    fn partial_runtime_files_use_explicit_versioned_defaults() {
        let config =
            RuntimeLoopConfigV1::from_toml("retry_budget = 4\n").expect("partial runtime config");
        assert_eq!(config.schema_version, RUNTIME_CONFIG_SCHEMA_VERSION);
        assert_eq!(config.retry_budget, 4);
        assert_eq!(config.tool_presentation, ToolPresentationConfig::Native);
        assert_eq!(config.timeout_millis, 120_000);
    }

    #[test]
    fn timeout_budget_envelope_fails_before_execution() {
        let config = RuntimeLoopConfigV1 {
            retry_budget: 8,
            replan_budget: 8,
            timeout_millis: 1_000,
            ..RuntimeLoopConfigV1::default()
        };
        let errors = compile_effective_config(
            config,
            BTreeMap::new(),
            RuntimeConfigHardLimits::default(),
            true,
        )
        .expect_err("budget envelope must be rejected");
        assert!(errors.iter().any(|error| error.contains("timeout_millis")));
    }

    #[test]
    fn explain_config_exposes_only_redacted_effective_identity() {
        let effective = compile_effective_config(
            RuntimeLoopConfigV1 {
                provider: Some("openai".to_owned()),
                model: Some("gpt-test".to_owned()),
                ..RuntimeLoopConfigV1::default()
            },
            BTreeMap::from([("provider".to_owned(), "session_override".to_owned())]),
            RuntimeConfigHardLimits::default(),
            true,
        )
        .expect("effective config");
        let explanation = explain_config(&effective);
        assert_eq!(explanation["schema_version"], RUNTIME_CONFIG_SCHEMA_VERSION);
        assert_eq!(explanation["fingerprint"], effective.fingerprint);
        assert_eq!(explanation["frozen_for_session"], true);
        assert_eq!(explanation["provenance"]["provider"], "session_override");
        assert!(
            explanation["fingerprint_inputs"]
                .as_array()
                .expect("fingerprint inputs")
                .iter()
                .any(|value| value == "tool_presentation")
        );
        assert_eq!(
            explanation["non_fingerprint_inputs"],
            json!(["diagnostics_enabled"])
        );
        assert!(explanation.get("secret").is_none());
    }
}
