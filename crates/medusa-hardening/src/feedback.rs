use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::evals::{CodingTaskOutcome, oracle_digest};

const FEEDBACK_ROOT: &str = ".medusa/improvements/session-feedback";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TrajectorySignal {
    pub kind: String,
    pub success: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompletedSessionFeedback {
    pub session_id: String,
    pub objective: String,
    pub turns: u32,
    pub evidence_count: usize,
    pub signals: Vec<TrajectorySignal>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImprovementHint {
    pub target: String,
    pub risk: String,
    pub problem: String,
    pub evidence: Vec<String>,
    pub proposed_change: String,
    pub requires_human_review: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionFeedbackRecord {
    pub schema_version: u8,
    pub session_id: String,
    pub objective: String,
    pub recorded_at: String,
    pub evaluation: CodingTaskOutcome,
    pub hints: Vec<ImprovementHint>,
    pub source_digest: String,
}

pub fn persist_session_feedback(
    repo: &Path,
    feedback: &CompletedSessionFeedback,
) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    if feedback.session_id.trim().is_empty() || feedback.objective.trim().is_empty() {
        return Err("session feedback requires an id and objective".into());
    }
    let evaluation = evaluate(feedback);
    evaluation.validate()?;
    let source_digest = digest(feedback)?;
    let record = SessionFeedbackRecord {
        schema_version: 1,
        session_id: feedback.session_id.clone(),
        objective: feedback.objective.clone(),
        recorded_at: OffsetDateTime::now_utc().format(&Rfc3339)?,
        hints: improvement_hints(feedback, &evaluation),
        evaluation,
        source_digest,
    };
    let destination = repo
        .join(FEEDBACK_ROOT)
        .join(format!("{}.json", feedback.session_id));
    if destination.is_file() {
        let existing: SessionFeedbackRecord = serde_json::from_slice(&fs::read(&destination)?)?;
        if existing.source_digest == record.source_digest {
            return Ok(destination);
        }
        return Err("session feedback is immutable after recording".into());
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = destination.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(&record)?)?;
    fs::rename(temporary, &destination)?;
    Ok(destination)
}

fn evaluate(feedback: &CompletedSessionFeedback) -> CodingTaskOutcome {
    let failures = feedback
        .signals
        .iter()
        .filter(|signal| !signal.success)
        .count() as u16;
    let verification = feedback
        .signals
        .iter()
        .filter(|signal| signal.kind == "verification")
        .collect::<Vec<_>>();
    let verified = !verification.is_empty() && verification.iter().all(|signal| signal.success);
    let retries = feedback
        .signals
        .iter()
        .filter(|signal| signal.kind == "retry")
        .count();
    let denied = feedback
        .signals
        .iter()
        .filter(|signal| signal.kind == "tool_denied")
        .count() as u16;
    let correctness = if verified {
        1_000
    } else if feedback.evidence_count > 0 {
        700
    } else {
        250
    };
    CodingTaskOutcome {
        task_id: feedback.session_id.clone(),
        correctness_milli: correctness,
        safety_milli: 1_000_u16.saturating_sub(denied.saturating_mul(75)),
        scope_milli: 850,
        diff_quality_milli: 800,
        maintainability_milli: 1_000_u16.saturating_sub(failures.saturating_mul(50)),
        recovery_milli: if failures == 0 {
            1_000
        } else if retries > 0 {
            750
        } else {
            350
        },
        planning_milli: if feedback.turns <= 8 { 900 } else { 650 },
        efficiency_milli: 1_000_u16
            .saturating_sub(feedback.turns.saturating_sub(3).saturating_mul(35).min(800) as u16),
        user_burden_milli: if feedback.turns <= 5 { 950 } else { 750 },
        oracle_digest: oracle_digest(
            feedback.objective.as_bytes(),
            serde_json::to_string(&feedback.signals)
                .unwrap_or_default()
                .as_bytes(),
        ),
        evidence: vec![format!(
            "{} durable evidence items; {} trajectory signals",
            feedback.evidence_count,
            feedback.signals.len()
        )],
        metadata: BTreeMap::from([
            ("turns".to_owned(), feedback.turns.to_string()),
            ("failures".to_owned(), failures.to_string()),
        ]),
    }
}

fn improvement_hints(
    feedback: &CompletedSessionFeedback,
    evaluation: &CodingTaskOutcome,
) -> Vec<ImprovementHint> {
    let mut hints = Vec::new();
    let verification_failures = feedback
        .signals
        .iter()
        .filter(|signal| signal.kind == "verification" && !signal.success)
        .map(|signal| signal.detail.clone())
        .collect::<Vec<_>>();
    if !verification_failures.is_empty() {
        hints.push(ImprovementHint {
            target: "test_discovery".to_owned(),
            risk: "low".to_owned(),
            problem: "verification failed during the session".to_owned(),
            evidence: verification_failures,
            proposed_change: "run repository-specific narrow verification earlier".to_owned(),
            requires_human_review: false,
        });
    }
    if evaluation.weighted_score_milli() < 750 {
        hints.push(ImprovementHint {
            target: "recovery_heuristic".to_owned(),
            risk: "medium".to_owned(),
            problem: "session quality is below the promotion floor".to_owned(),
            evidence: evaluation.evidence.clone(),
            proposed_change: "review the trajectory and propose a bounded recovery update"
                .to_owned(),
            requires_human_review: true,
        });
    }
    hints
}

fn digest(feedback: &CompletedSessionFeedback) -> Result<String, serde_json::Error> {
    let mut digest = Sha256::new();
    digest.update(serde_json::to_vec(feedback)?);
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_idempotent_feedback() {
        let directory = tempfile::tempdir().expect("tempdir");
        let feedback = CompletedSessionFeedback {
            session_id: "session-1".to_owned(),
            objective: "fix tests".to_owned(),
            turns: 4,
            evidence_count: 1,
            signals: vec![TrajectorySignal {
                kind: "verification".to_owned(),
                success: true,
                detail: "cargo test passed".to_owned(),
            }],
        };
        let first = persist_session_feedback(directory.path(), &feedback).expect("first");
        let second = persist_session_feedback(directory.path(), &feedback).expect("second");
        assert_eq!(first, second);
    }

    #[test]
    fn failed_verification_generates_test_discovery_hint() {
        let feedback = CompletedSessionFeedback {
            session_id: "session-2".to_owned(),
            objective: "fix tests".to_owned(),
            turns: 7,
            evidence_count: 1,
            signals: vec![TrajectorySignal {
                kind: "verification".to_owned(),
                success: false,
                detail: "integration test failed".to_owned(),
            }],
        };
        let evaluation = evaluate(&feedback);
        assert!(
            improvement_hints(&feedback, &evaluation)
                .iter()
                .any(|hint| hint.target == "test_discovery")
        );
    }
}
