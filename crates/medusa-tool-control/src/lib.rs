#![forbid(unsafe_code)]

use medusa_core::{ErrorCategory, MedusaError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum FailureClass {
    Transient,
    Authentication,
    InvalidRequest,
    CapabilityUnavailable,
    ResourceExhausted,
    Deterministic,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RetryAction {
    Retry,
    Replan,
    Stop,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetryDecision {
    pub class: FailureClass,
    pub action: RetryAction,
    pub attempt: u8,
    pub max_attempts: u8,
    pub rationale: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetryGuard {
    max_attempts: u8,
    attempts: u8,
    signatures: BTreeMap<String, u8>,
}

impl RetryGuard {
    /// Creates a guard allowing up to `max_attempts` total attempts,
    /// counting the first attempt as attempt 1. A limit of zero is
    /// normalized to one (a single attempt, no retries) for backward
    /// compatibility; prefer [`RetryGuard::try_new`] to reject zero
    /// explicitly.
    #[must_use]
    pub fn new(max_attempts: u8) -> Self {
        Self {
            max_attempts: max_attempts.max(1),
            attempts: 0,
            signatures: BTreeMap::new(),
        }
    }

    /// Strict constructor: `max_attempts` counts total attempts including
    /// the first, and zero is rejected. This matches `DynamicSchedule::new`
    /// and `RetryPolicy::validate`, which both treat zero as invalid, and
    /// the provider contract where `max_retries = 0` means a single attempt.
    pub fn try_new(max_attempts: u8) -> Result<Self, &'static str> {
        if max_attempts == 0 {
            return Err("max_attempts must be greater than zero");
        }
        Ok(Self::new(max_attempts))
    }

    pub fn begin_attempt(&mut self, signature: &str) -> Result<u8, &'static str> {
        if self.attempts >= self.max_attempts {
            return Err("tool retry budget exhausted");
        }
        self.attempts = self.attempts.saturating_add(1);
        let count = self.signatures.entry(signature.to_owned()).or_default();
        *count = count.saturating_add(1);
        if *count > 1 {
            return Err("loop guard blocked an unchanged repeated attempt");
        }
        Ok(self.attempts)
    }

    /// Classifies a structured [`MedusaError`] and renders the retry
    /// decision. Prefer this over [`RetryGuard::decide`] whenever the
    /// failure already carries a category and retry flag.
    #[must_use]
    pub fn decide_structured(&self, error: &MedusaError) -> RetryDecision {
        let class = classify_medusa_error(error);
        let action = match class {
            FailureClass::Transient if self.attempts < self.max_attempts => RetryAction::Retry,
            FailureClass::Transient | FailureClass::Unknown => RetryAction::Replan,
            FailureClass::Authentication
            | FailureClass::InvalidRequest
            | FailureClass::CapabilityUnavailable
            | FailureClass::ResourceExhausted
            | FailureClass::Deterministic => RetryAction::Stop,
        };
        RetryDecision {
            class,
            action,
            attempt: self.attempts,
            max_attempts: self.max_attempts,
            rationale: match action {
                RetryAction::Retry => "transient failure permits one bounded retry".to_owned(),
                RetryAction::Replan => {
                    "retrying unchanged would not be justified; change route or strategy".to_owned()
                }
                RetryAction::Stop => {
                    "deterministic or non-retryable failure must not be repeated".to_owned()
                }
            },
        }
    }

    #[must_use]
    pub fn decide(&self, error: &str) -> RetryDecision {
        let class = classify_failure(error);
        let action = match class {
            FailureClass::Transient if self.attempts < self.max_attempts => RetryAction::Retry,
            FailureClass::Transient | FailureClass::Unknown => RetryAction::Replan,
            FailureClass::Authentication
            | FailureClass::InvalidRequest
            | FailureClass::CapabilityUnavailable
            | FailureClass::ResourceExhausted
            | FailureClass::Deterministic => RetryAction::Stop,
        };
        RetryDecision {
            class,
            action,
            attempt: self.attempts,
            max_attempts: self.max_attempts,
            rationale: match action {
                RetryAction::Retry => "transient failure permits one bounded retry".to_owned(),
                RetryAction::Replan => {
                    "retrying unchanged would not be justified; change route or strategy".to_owned()
                }
                RetryAction::Stop => {
                    "deterministic or non-retryable failure must not be repeated".to_owned()
                }
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum VerificationRequirement {
    Syntax,
    Focused,
    Package,
    Workspace,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerificationPlan {
    pub requirements: Vec<VerificationRequirement>,
    pub rationale: Vec<String>,
}

#[must_use]
pub fn verification_plan(objective: &str) -> VerificationPlan {
    let lower = objective.to_ascii_lowercase();
    let mut requirements = vec![VerificationRequirement::Syntax];
    let mut rationale = vec!["all mutations require syntax or format validation".to_owned()];
    if ["fix", "implement", "edit", "refactor", "change"]
        .iter()
        .any(|marker| lower.contains(marker))
    {
        requirements.push(VerificationRequirement::Focused);
        rationale.push("code mutation requires focused behavioral verification".to_owned());
    }
    if ["dependency", "package", "crate", "module"]
        .iter()
        .any(|marker| lower.contains(marker))
    {
        requirements.push(VerificationRequirement::Package);
        rationale.push("package-boundary change requires package-level verification".to_owned());
    }
    if [
        "workspace",
        "repository-wide",
        "architecture",
        "release",
        "all tests",
        "ci",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        requirements.push(VerificationRequirement::Workspace);
        rationale.push(
            "cross-cutting or release-sensitive change requires workspace verification".to_owned(),
        );
    }
    requirements.dedup();
    VerificationPlan {
        requirements,
        rationale,
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionTrace {
    pub fingerprint: String,
    pub attempt: u8,
    pub failure_class: Option<FailureClass>,
    pub action: Option<RetryAction>,
    pub verification: Vec<VerificationRequirement>,
}

#[must_use]
pub fn trace(
    signature: &str,
    attempt: u8,
    decision: Option<&RetryDecision>,
    verification: &VerificationPlan,
) -> ExecutionTrace {
    ExecutionTrace {
        fingerprint: format!("{:x}", Sha256::digest(signature.as_bytes())),
        attempt,
        failure_class: decision.map(|value| value.class),
        action: decision.map(|value| value.action),
        verification: verification.requirements.clone(),
    }
}

/// Classifies a structured [`MedusaError`] without scraping message text.
///
/// Structured signals take precedence: an error marked `retryable` (or
/// carrying the [`ErrorCategory::Transient`] category) is [`FailureClass::Transient`];
/// [`ErrorCategory::Validation`] maps to [`FailureClass::InvalidRequest`], and
/// [`ErrorCategory::Policy`] maps to [`FailureClass::Deterministic`] (a denial
/// will not succeed unchanged).
/// Every other category consults [`classify_failure`] on the message as a
/// fallback so unmapped failures keep their historical behavior.
#[must_use]
pub fn classify_medusa_error(error: &MedusaError) -> FailureClass {
    if error.retryable || error.category == ErrorCategory::Transient {
        return FailureClass::Transient;
    }
    if error.category == ErrorCategory::Validation {
        return FailureClass::InvalidRequest;
    }
    if error.category == ErrorCategory::Policy {
        return FailureClass::Deterministic;
    }
    classify_failure(&error.message)
}

#[must_use]
pub fn classify_failure(error: &str) -> FailureClass {
    let lower = error.to_ascii_lowercase();
    if [
        "timeout",
        "temporarily",
        "connection reset",
        "rate limit",
        "429",
        "503",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        FailureClass::Transient
    } else if [
        "unauthorized",
        "forbidden",
        "credential",
        "api key",
        "401",
        "403",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        FailureClass::Authentication
    } else if [
        "invalid argument",
        "schema",
        "malformed",
        "bad request",
        "400",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        FailureClass::InvalidRequest
    } else if ["unsupported", "not available", "not implemented"]
        .iter()
        .any(|marker| lower.contains(marker))
    {
        FailureClass::CapabilityUnavailable
    } else if ["out of memory", "resource exhausted", "too large"]
        .iter()
        .any(|marker| lower.contains(marker))
    {
        FailureClass::ResourceExhausted
    } else if [
        "compile",
        "test failed",
        "assertion",
        "permission denied",
        "conflict",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        FailureClass::Deterministic
    } else {
        FailureClass::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_failure_gets_one_bounded_retry() {
        let mut guard = RetryGuard::new(2);
        assert_eq!(guard.begin_attempt("provider:request:1"), Ok(1));
        let decision = guard.decide("request timeout");
        assert_eq!(decision.action, RetryAction::Retry);
    }

    #[test]
    fn deterministic_failure_is_not_retried() {
        let mut guard = RetryGuard::new(2);
        guard.begin_attempt("provider:request:1").unwrap();
        let decision = guard.decide("test failed: assertion mismatch");
        assert_eq!(decision.action, RetryAction::Stop);
    }

    #[test]
    fn strict_constructor_rejects_zero_attempts() {
        assert!(RetryGuard::try_new(0).is_err());
        assert!(RetryGuard::try_new(1).is_ok());
    }

    #[test]
    fn legacy_zero_limit_normalizes_to_a_single_attempt() {
        let mut guard = RetryGuard::new(0);
        assert_eq!(guard.begin_attempt("only"), Ok(1));
        assert!(guard.begin_attempt("second").is_err());
    }

    #[test]
    fn structured_transient_error_is_retried() {
        use medusa_core::MedusaError;
        let mut guard = RetryGuard::try_new(2).expect("valid budget");
        guard.begin_attempt("provider:request:1").unwrap();
        let io_error = MedusaError::from(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "timed out",
        ));
        assert!(io_error.retryable);
        let decision = guard.decide_structured(&io_error);
        assert_eq!(decision.class, FailureClass::Transient);
        assert_eq!(decision.action, RetryAction::Retry);
    }

    #[test]
    fn corrupt_frame_is_retried_nowhere_across_layers() {
        // Cross-layer contract: a corrupt frame converts to a non-retryable
        // core error AND classifies to a non-transient tool-control class,
        // so no layer retries it.
        use medusa_core::MedusaError;
        let frame: Result<serde_json::Value, serde_json::Error> =
            serde_json::from_str("{truncated");
        let core_error = MedusaError::from(frame.expect_err("corrupt frame"));
        assert!(!core_error.retryable);
        assert_eq!(
            classify_medusa_error(&core_error),
            FailureClass::InvalidRequest
        );
        let mut guard = RetryGuard::try_new(3).expect("valid budget");
        guard.begin_attempt("frame:1").unwrap();
        let decision = guard.decide_structured(&core_error);
        assert_eq!(decision.action, RetryAction::Stop);
    }

    #[test]
    fn structured_signals_win_over_message_text() {
        use medusa_core::{ErrorCategory, ErrorCode, MedusaError};
        // Message mentions timeout, but the structured denial is explicit.
        let denied = MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            "request timeout is not permitted by policy",
        );
        assert_eq!(classify_medusa_error(&denied), FailureClass::Deterministic);
    }

    #[test]
    fn unchanged_attempt_is_blocked() {
        let mut guard = RetryGuard::new(3);
        guard.begin_attempt("same").unwrap();
        assert_eq!(
            guard.begin_attempt("same"),
            Err("loop guard blocked an unchanged repeated attempt")
        );
    }

    #[test]
    fn repository_wide_change_requires_workspace_verification() {
        let plan = verification_plan("Refactor the architecture repository-wide and run CI");
        assert!(
            plan.requirements
                .contains(&VerificationRequirement::Workspace)
        );
        assert!(
            plan.requirements
                .contains(&VerificationRequirement::Focused)
        );
    }
}
