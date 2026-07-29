#![forbid(unsafe_code)]

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
    #[must_use]
    pub fn new(max_attempts: u8) -> Self {
        Self {
            max_attempts: max_attempts.max(1),
            attempts: 0,
            signatures: BTreeMap::new(),
        }
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
