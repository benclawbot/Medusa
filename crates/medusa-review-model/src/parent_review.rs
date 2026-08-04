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

pub fn parse_parent_review_response(
    text: &str,
) -> Result<ParentReviewOutcome, ParentReviewResponseError> {
    let final_line = text
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or(ParentReviewResponseError::EmptyResponse)?;
    let response: ParentReviewResponse = serde_json::from_str(final_line)
        .map_err(|error| ParentReviewResponseError::InvalidEnvelope(error.to_string()))?;
    if response.schema_version != PARENT_REVIEW_SCHEMA_VERSION {
        return Err(ParentReviewResponseError::UnsupportedSchema(
            response.schema_version,
        ));
    }
    let rationale = response.rationale.trim();
    if rationale.is_empty() {
        return Err(ParentReviewResponseError::EmptyRationale);
    }
    let response = ParentReviewResponse {
        schema_version: response.schema_version,
        decision: response.decision,
        rationale: rationale.to_owned(),
    };
    let response_fingerprint = fingerprint(&response);
    Ok(ParentReviewOutcome {
        schema_version: response.schema_version,
        decision: response.decision,
        rationale: response.rationale,
        response_fingerprint,
    })
}

fn fingerprint(response: &ParentReviewResponse) -> String {
    let bytes = serde_json::to_vec(response).unwrap_or_default();
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_versioned_final_json_after_human_summary() {
        let outcome = parse_parent_review_response(
            "The prepared patch matches the verified scope.\n{\"schema_version\":1,\"decision\":\"accepted\",\"rationale\":\"exact patch and evidence agree\"}",
        )
        .expect("typed review");
        assert_eq!(outcome.decision, ParentReviewDecision::Accepted);
        assert_eq!(outcome.rationale, "exact patch and evidence agree");
        assert_eq!(outcome.response_fingerprint.len(), 64);
    }

    #[test]
    fn accepts_typed_revision_request() {
        let outcome = parse_parent_review_response(
            "{\"schema_version\":1,\"decision\":\"revision_requested\",\"rationale\":\"missing regression coverage\"}",
        )
        .expect("typed revision");
        assert_eq!(
            outcome.decision,
            ParentReviewDecision::RevisionRequested
        );
    }

    #[test]
    fn free_form_marker_cannot_authorize_integration() {
        let error = parse_parent_review_response(
            "MEDUSA_REVIEW_ACCEPTED: exact patch and evidence agree",
        )
        .expect_err("marker must fail closed");
        assert!(matches!(
            error,
            ParentReviewResponseError::InvalidEnvelope(_)
        ));
    }

    #[test]
    fn unknown_fields_and_trailing_text_fail_closed() {
        assert!(parse_parent_review_response(
            "{\"schema_version\":1,\"decision\":\"accepted\",\"rationale\":\"ok\",\"extra\":true}"
        )
        .is_err());
        assert!(parse_parent_review_response(
            "{\"schema_version\":1,\"decision\":\"accepted\",\"rationale\":\"ok\"}\ntrailing"
        )
        .is_err());
    }

    #[test]
    fn unsupported_schema_and_empty_rationale_fail_closed() {
        assert_eq!(
            parse_parent_review_response(
                "{\"schema_version\":2,\"decision\":\"accepted\",\"rationale\":\"ok\"}"
            ),
            Err(ParentReviewResponseError::UnsupportedSchema(2))
        );
        assert_eq!(
            parse_parent_review_response(
                "{\"schema_version\":1,\"decision\":\"accepted\",\"rationale\":\"   \"}"
            ),
            Err(ParentReviewResponseError::EmptyRationale)
        );
    }
}
