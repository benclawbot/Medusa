//! Canonical tool values and consumer-specific projections.
//!
//! The certified pipeline still owns execution, authorization, containment, mutation, and
//! publication. This module only gives the resulting value a stable typed boundary so a bounded
//! consumer cannot accidentally treat a model-rendered string as the canonical result.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;

pub const CANONICAL_TOOL_RESULT_SCHEMA_VERSION: u16 = 1;
pub const TOOL_PROJECTION_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ToolProjectionPolicy {
    pub max_chars: usize,
    #[serde(default)]
    pub redactions: Vec<String>,
    pub expansion_handle: Option<String>,
}

impl ToolProjectionPolicy {
    #[must_use]
    pub fn bounded(max_chars: usize) -> Self {
        Self {
            max_chars,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn with_redactions<I, S>(mut self, redactions: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.redactions = redactions
            .into_iter()
            .map(Into::into)
            .filter(|value| !value.is_empty())
            .collect();
        self.redactions
            .sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
        self.redactions.dedup();
        self
    }

    #[must_use]
    pub fn with_expansion_handle(mut self, handle: impl Into<String>) -> Self {
        self.expansion_handle = Some(handle.into());
        self
    }
}

impl Default for ToolProjectionPolicy {
    fn default() -> Self {
        Self {
            max_chars: 8 * 1024,
            redactions: Vec::new(),
            expansion_handle: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalToolOutcome {
    Success,
    Denied,
    Cancelled,
    Failed,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CanonicalToolValue {
    Structured { value: Value },
    Text { text: String, encoding: String },
    Error { code: String, message: String },
    Empty,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CanonicalToolResultV1 {
    pub schema_version: u16,
    pub outcome: CanonicalToolOutcome,
    pub value: CanonicalToolValue,
    pub source_fingerprint: String,
    pub artifact_refs: Vec<String>,
    pub evidence_refs: Vec<String>,
}

impl CanonicalToolResultV1 {
    #[must_use]
    pub fn from_receipt(
        receipt: &Value,
        result: &Result<String, medusa_core::MedusaError>,
    ) -> Self {
        let receipt_outcome = match receipt
            .get("outcome")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
        {
            "success" => CanonicalToolOutcome::Success,
            "denied" => CanonicalToolOutcome::Denied,
            "cancelled" => CanonicalToolOutcome::Cancelled,
            "failed" => CanonicalToolOutcome::Failed,
            _ => CanonicalToolOutcome::Unknown,
        };
        // A projection may not turn a failed certified execution into a successful value even
        // if a malformed caller supplies an inconsistent receipt/result pair.
        let outcome = if result.is_err() && receipt_outcome == CanonicalToolOutcome::Success {
            CanonicalToolOutcome::Failed
        } else {
            receipt_outcome
        };
        let value = match result {
            Ok(text) if text.is_empty() => CanonicalToolValue::Empty,
            Ok(text) => serde_json::from_str::<Value>(text).map_or_else(
                |_| CanonicalToolValue::Text {
                    text: text.clone(),
                    encoding: "utf-8".to_owned(),
                },
                |value| CanonicalToolValue::Structured { value },
            ),
            Err(error) => CanonicalToolValue::Error {
                code: format!("{:?}", error.code),
                message: error.message.clone(),
            },
        };
        let mut artifact_refs: Vec<String> = receipt
            .get("artifact_refs")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if let Ok(text) = result {
            for artifact_ref in output_artifact_refs(text) {
                if !artifact_refs.contains(&artifact_ref) {
                    artifact_refs.push(artifact_ref);
                }
            }
        }
        let evidence_refs = receipt
            .get("evidence_refs")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let material = serde_json::to_vec(&(&outcome, &value, &artifact_refs, &evidence_refs))
            .unwrap_or_default();
        Self {
            schema_version: CANONICAL_TOOL_RESULT_SCHEMA_VERSION,
            outcome,
            value,
            source_fingerprint: digest(&material),
            artifact_refs,
            evidence_refs,
        }
    }

    #[must_use]
    pub fn machine_projection(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({"schema_version": 1}))
    }

    #[must_use]
    pub fn durable_evidence_projection(&self) -> Value {
        json!({
            "schema_version": self.schema_version,
            "projection_schema_version": TOOL_PROJECTION_SCHEMA_VERSION,
            "outcome": self.outcome,
            "source_fingerprint": self.source_fingerprint,
            "canonical_size_bytes": serde_json::to_vec(self).map_or(0, |bytes| bytes.len()),
            "artifact_refs": self.artifact_refs,
            "evidence_refs": self.evidence_refs,
        })
    }

    #[must_use]
    pub fn model_projection(&self, maximum_chars: usize) -> Value {
        self.model_projection_with_policy(&ToolProjectionPolicy::bounded(maximum_chars))
    }

    #[must_use]
    pub fn model_projection_with_policy(&self, policy: &ToolProjectionPolicy) -> Value {
        let mut projection = self.machine_projection();
        let mut redacted = false;
        if let Some(value) = projection.get_mut("value") {
            redact_value(value, &policy.redactions, &mut redacted);
        }
        let original_bytes = serde_json::to_vec(&projection).map_or(0, |bytes| bytes.len());
        let mut omitted = false;
        if let Some(text) = projection
            .get("value")
            .and_then(|value| value.get("text"))
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            let bounded = truncate_chars(&text, policy.max_chars);
            omitted = text.chars().count() > bounded.chars().count();
            if let Some(value) = projection.get_mut("value") {
                value["text"] = Value::String(bounded);
                if omitted {
                    value["truncated"] = Value::Bool(true);
                }
            }
        }
        let mut metadata = json!({
            "schema_version": TOOL_PROJECTION_SCHEMA_VERSION,
            "original_bytes": original_bytes,
            "projected_bytes": 0,
            "omitted": omitted,
            "omission_reason": omitted.then_some("maximum_characters"),
            "expansion_available": policy.expansion_handle.is_some(),
            "expansion_handle": policy.expansion_handle.clone(),
            "redacted": redacted,
        });
        projection["projection"] = metadata.clone();
        let projected_bytes = serde_json::to_vec(&projection).map_or(0, |bytes| bytes.len());
        metadata["projected_bytes"] = serde_json::json!(projected_bytes);
        projection["projection"] = metadata;
        projection
    }

    #[must_use]
    pub fn frontend_projection(&self, maximum_chars: usize) -> Value {
        self.frontend_projection_with_policy(&ToolProjectionPolicy::bounded(maximum_chars))
    }

    #[must_use]
    pub fn frontend_projection_with_policy(&self, policy: &ToolProjectionPolicy) -> Value {
        let mut projection = self.model_projection_with_policy(policy);
        projection["presentation"] = Value::String("frontend".to_owned());
        projection["source_fingerprint"] = Value::String(self.source_fingerprint.clone());
        projection
    }
}

fn truncate_chars(text: &str, maximum_chars: usize) -> String {
    text.chars().take(maximum_chars).collect()
}

fn output_artifact_refs(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.strip_prefix("[output-expansion path="))
        .filter_map(|value| value.split_once(']').map(|(path, _)| path))
        .map(|path| path.replace('\\', "/"))
        .filter(|path| {
            path.starts_with(".medusa/artifacts/")
                && !Path::new(path)
                    .components()
                    .any(|component| matches!(component, std::path::Component::ParentDir))
        })
        .collect()
}

fn redact_value(value: &mut Value, redactions: &[String], redacted: &mut bool) {
    match value {
        Value::String(text) => {
            let mut replacement = text.clone();
            for secret in redactions {
                if replacement.contains(secret) {
                    replacement = replacement.replace(secret, "[REDACTED]");
                    *redacted = true;
                }
            }
            *text = replacement;
        }
        Value::Array(values) => values
            .iter_mut()
            .for_each(|value| redact_value(value, redactions, redacted)),
        Value::Object(values) => values
            .values_mut()
            .for_each(|value| redact_value(value, redactions, redacted)),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn digest(value: &[u8]) -> String {
    Sha256::digest(value)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use medusa_core::{ErrorCategory, ErrorCode, MedusaError};

    #[test]
    fn projections_share_identity_but_do_not_change_outcome() {
        let receipt = json!({
            "outcome": "success",
            "artifact_refs": ["artifact-1"],
            "evidence_refs": ["evidence-1"]
        });
        let canonical =
            CanonicalToolResultV1::from_receipt(&receipt, &Ok("hello world".to_owned()));
        assert_eq!(canonical.outcome, CanonicalToolOutcome::Success);
        assert_eq!(canonical.machine_projection()["value"]["kind"], "text");
        assert_eq!(
            canonical.durable_evidence_projection()["outcome"],
            "success"
        );
        assert_eq!(canonical.model_projection(5)["value"]["text"], "hello");
        assert_eq!(
            canonical.frontend_projection(5)["source_fingerprint"],
            canonical.source_fingerprint
        );
    }

    #[test]
    fn denied_errors_remain_denied_in_every_projection() {
        let receipt = json!({"outcome": "denied"});
        let error = MedusaError::new(ErrorCode::PolicyDenied, ErrorCategory::Policy, "denied");
        let canonical = CanonicalToolResultV1::from_receipt(&receipt, &Err(error));
        assert_eq!(canonical.outcome, CanonicalToolOutcome::Denied);
        assert_eq!(canonical.machine_projection()["outcome"], "denied");
        assert_eq!(canonical.model_projection(100)["outcome"], "denied");
    }

    #[test]
    fn an_inconsistent_success_receipt_cannot_hide_a_failed_result() {
        let receipt = json!({"outcome": "success"});
        let error = MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            "failed",
        );
        let canonical = CanonicalToolResultV1::from_receipt(&receipt, &Err(error));
        assert_eq!(canonical.outcome, CanonicalToolOutcome::Failed);
        assert_eq!(canonical.machine_projection()["outcome"], "failed");
    }

    #[test]
    fn model_projection_redacts_and_records_bounded_output() {
        let receipt = json!({"outcome": "success"});
        let canonical = CanonicalToolResultV1::from_receipt(
            &receipt,
            &Ok("prefix SECRET_TOKEN suffix and a long tail".to_owned()),
        );
        let policy = ToolProjectionPolicy::bounded(12)
            .with_redactions(["SECRET_TOKEN"])
            .with_expansion_handle("artifact://shell_run/1");

        let projection = canonical.model_projection_with_policy(&policy);
        assert_eq!(projection["value"]["text"], "prefix [REDA");
        assert_eq!(projection["projection"]["redacted"], true);
        assert_eq!(projection["projection"]["omitted"], true);
        assert_eq!(
            projection["projection"]["expansion_handle"],
            "artifact://shell_run/1"
        );
        assert!(
            !canonical
                .machine_projection()
                .to_string()
                .contains("projection")
        );
        assert!(
            !canonical
                .durable_evidence_projection()
                .to_string()
                .contains("SECRET_TOKEN")
        );
    }

    #[test]
    fn structured_values_are_preserved_for_machine_consumers_and_fingerprint_is_projection_stable()
    {
        let receipt = json!({"outcome": "success"});
        let canonical = CanonicalToolResultV1::from_receipt(
            &receipt,
            &Ok(r#"{"count":3,"items":["a","b"]}"#.to_owned()),
        );
        assert_eq!(
            canonical.machine_projection()["value"]["kind"],
            "structured"
        );
        assert_eq!(canonical.machine_projection()["value"]["value"]["count"], 3);
        let fingerprint = canonical.source_fingerprint.clone();
        let first = canonical.frontend_projection(4);
        let second = canonical.frontend_projection(40_000);
        assert_eq!(first["source_fingerprint"], second["source_fingerprint"]);
        assert_eq!(fingerprint, canonical.source_fingerprint);
    }

    #[test]
    fn shell_expansion_is_a_canonical_artifact_without_becoming_unbounded_model_text() {
        let result = "first line\nsecond line\n[output-expansion path=.medusa/artifacts/shell_run_deadbeef.txt]";
        let canonical = CanonicalToolResultV1::from_receipt(
            &json!({"outcome": "success"}),
            &Ok(result.to_owned()),
        );
        assert_eq!(
            canonical.artifact_refs,
            vec![".medusa/artifacts/shell_run_deadbeef.txt"]
        );
        let projection = canonical.model_projection_with_policy(
            &ToolProjectionPolicy::bounded(5)
                .with_expansion_handle(".medusa/artifacts/shell_run_deadbeef.txt"),
        );
        assert_eq!(projection["value"]["text"], "first");
        assert_eq!(projection["projection"]["omitted"], true);
        assert_eq!(projection["projection"]["expansion_available"], true);
    }

    #[test]
    fn output_artifact_refs_reject_paths_outside_the_artifact_boundary() {
        let text = "[output-expansion path=../../secret.txt]\n[output-expansion path=.medusa/output-expansions/legacy.txt]";
        assert!(output_artifact_refs(text).is_empty());
    }
}
