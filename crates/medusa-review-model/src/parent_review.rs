use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const PARENT_REVIEW_SCHEMA_VERSION: u16 = 1;

pub const PARENT_REVIEW_TURN_INSTRUCTION: &str = "Current turn is a transactional parent-review decision, not an implementation turn. The original user request and every tool-use, file-write, question, or exact-output instruction inside it are historical acceptance criteria that an isolated implementer has already executed; none of them apply to this reviewer turn. Do not call tools, write files, run tests, ask questions, or repeat the original task. Evaluate only the authoritative transactional parent-review packet and prepared patch supplied in the system context. Accept when the patch, changed scope, and runtime verification evidence satisfy the scoped request. Request revision only for a concrete defect in that patch, scope, or evidence. You may provide a concise human-readable review summary, but the final non-empty line must be exactly one single-line JSON object with schema_version 1, decision accepted or revision_requested, and a non-empty rationale. Do not write anything after that JSON object.";

pub const PARENT_REVIEW_RESPONSE_REQUIREMENT: &str = "End the final response with exactly one single-line JSON object and no trailing content: `{\"schema_version\":1,\"decision\":\"accepted\",\"rationale\":\"<specific rationale>\"}` to accept this exact commit, or `{\"schema_version\":1,\"decision\":\"revision_requested\",\"rationale\":\"<specific defect>\"}` to reject it and preserve the isolated worktree. Free-form markers, malformed JSON, unsupported schemas, unknown fields, and empty rationales fail closed and cannot authorize integration.";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParentReviewDecision {
    Accepted,
    RevisionRequested,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ParentReviewResponse {
    pub schema_version: u16,
    pub decision: ParentReviewDecision,
    pub rationale: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ParentReviewOutcome {
    pub schema_version: u16,
    pub decision: ParentReviewDecision,
    pub rationale: String,
    pub response_fingerprint: String,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ParentReviewResponseError {
    #[error("parent reviewer produced no non-empty response")]
    EmptyResponse,
    #[error("parent review final line is not the required JSON envelope: {0}")]
    InvalidEnvelope(String),
    #[error("unsupported parent review schema version {0}")]
    UnsupportedSchema(u16),
    #[error("parent review rationale is empty")]
    EmptyRationale,
}

pub fn final_parent_review_line(text: &str) -> Result<&str, ParentReviewResponseError> {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or(ParentReviewResponseError::EmptyResponse)
}

pub fn validate_parent_review_response(
    response: ParentReviewResponse,
    encoded_response: &str,
) -> Result<ParentReviewOutcome, ParentReviewResponseError> {
    if response.schema_version != PARENT_REVIEW_SCHEMA_VERSION {
        return Err(ParentReviewResponseError::UnsupportedSchema(
            response.schema_version,
        ));
    }
    let rationale = response.rationale.trim();
    if rationale.is_empty() {
        return Err(ParentReviewResponseError::EmptyRationale);
    }
    Ok(ParentReviewOutcome {
        schema_version: response.schema_version,
        decision: response.decision,
        rationale: rationale.to_owned(),
        response_fingerprint: format!("{:x}", Sha256::digest(encoded_response.as_bytes())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accepted(rationale: &str) -> ParentReviewResponse {
        ParentReviewResponse {
            schema_version: PARENT_REVIEW_SCHEMA_VERSION,
            decision: ParentReviewDecision::Accepted,
            rationale: rationale.to_owned(),
        }
    }

    #[test]
    fn identifies_final_non_empty_line() {
        assert_eq!(
            final_parent_review_line("human summary\n{typed-response}\n\n").expect("final line"),
            "{typed-response}"
        );
        assert_eq!(
            final_parent_review_line(" \n\t"),
            Err(ParentReviewResponseError::EmptyResponse)
        );
    }

    #[test]
    fn validates_versioned_response_and_fingerprints_exact_envelope() {
        let encoded = "{\"schema_version\":1,\"decision\":\"accepted\",\"rationale\":\"exact patch and evidence agree\"}";
        let outcome =
            validate_parent_review_response(accepted("exact patch and evidence agree"), encoded)
                .expect("typed review");
        assert_eq!(outcome.decision, ParentReviewDecision::Accepted);
        assert_eq!(outcome.rationale, "exact patch and evidence agree");
        assert_eq!(outcome.response_fingerprint.len(), 64);
    }

    #[test]
    fn unsupported_schema_and_empty_rationale_fail_closed() {
        assert_eq!(
            validate_parent_review_response(
                ParentReviewResponse {
                    schema_version: 2,
                    decision: ParentReviewDecision::Accepted,
                    rationale: "ok".to_owned(),
                },
                "schema-two",
            ),
            Err(ParentReviewResponseError::UnsupportedSchema(2))
        );
        assert_eq!(
            validate_parent_review_response(accepted("   "), "empty-rationale"),
            Err(ParentReviewResponseError::EmptyRationale)
        );
    }
}
