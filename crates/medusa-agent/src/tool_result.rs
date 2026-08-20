//! Canonical tool values and consumer-specific projections.
//!
//! The certified pipeline still owns execution, authorization, containment, mutation, and
//! publication. This module only gives the resulting value a stable typed boundary so a bounded
//! consumer cannot accidentally treat a model-rendered string as the canonical result.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const CANONICAL_TOOL_RESULT_SCHEMA_VERSION: u16 = 1;

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
        let artifact_refs = receipt
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
            "outcome": self.outcome,
            "source_fingerprint": self.source_fingerprint,
            "artifact_refs": self.artifact_refs,
            "evidence_refs": self.evidence_refs,
        })
    }

    #[must_use]
    pub fn model_projection(&self, maximum_chars: usize) -> Value {
        let mut projection = self.machine_projection();
        let text = projection
            .get("value")
            .and_then(|value| value.get("text"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(text) = text {
            let bounded = text.chars().take(maximum_chars).collect::<String>();
            projection["value"]["text"] = Value::String(bounded);
            projection["value"]["truncated"] = Value::Bool(text.chars().count() > maximum_chars);
        }
        projection
    }

    #[must_use]
    pub fn frontend_projection(&self, maximum_chars: usize) -> Value {
        let mut projection = self.model_projection(maximum_chars);
        projection["source_fingerprint"] = Value::String(self.source_fingerprint.clone());
        projection
    }
}

fn digest(value: &[u8]) -> String {
    Sha256::digest(value)
        .iter()
        .map(|byte| format!("{:02x}", byte))
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
}
