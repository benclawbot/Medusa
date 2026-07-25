//! Closed-loop ingestion of completed coding sessions into durable evaluation and improvement evidence.

use std::{
    fs,
    path::{Path, PathBuf},
};

use medusa_evals::CodingTaskOutcome;
use medusa_improvement::{ImprovementRisk, ImprovementTarget, TrajectoryAnalysis, TrajectoryEvent, analyze_trajectory};
use medusa_protocol::EventPayload;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const FEEDBACK_ROOT: &str = ".medusa/improvements/session-feedback";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionFeedbackInput {
    pub session_id: String,
    pub objective: String,
    pub completed: bool,
    pub turns: u32,
    pub evidence_count: usize,
    pub events: Vec<EventPayload>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImprovementRecommendation {
    pub target: ImprovementTarget,
    pub risk: ImprovementRisk,
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
    pub trajectory: TrajectoryAnalysis,
    pub evaluation: CodingTaskOutcome,
    pub recommendations: Vec<ImprovementRecommendation>,
    pub source_digest: String,
}

pub fn record_completed_session(
    repo: &Path,
    input: &SessionFeedbackInput,
) -> Result<Option<PathBuf>, Box<dyn std::error::Error + Send + Sync>> {
    if !input.completed {
        return Ok(None);
    }
    if input.session_id.trim().is_empty() || input.objective.trim().is_empty() {
        return Err("completed-session feedback requires a session id and objective".into());
    }

    let trajectory_events = normalize_events(&input.events);
    let trajectory = analyze_trajectory(&trajectory_events);
    let evaluation = evaluate_session(input, &trajectory);
    evaluation.validate()?;
    let recommendations = recommendations(&trajectory, &evaluation);
    let source_digest = source_digest(input)?;
    let recorded_at = OffsetDateTime::now_utc().format(&Rfc3339)?;
    let record = SessionFeedbackRecord {
        schema_version: 1,
        session_id: input.session_id.clone(),
        objective: input.objective.clone(),
        recorded_at,
        trajectory,
        evaluation,
        recommendations,
        source_digest,
    };

    let destination = repo
        .join(FEEDBACK_ROOT)
        .join(format!("{}.json", input.session_id));
    if destination.is_file() {
        let existing: SessionFeedbackRecord = serde_json::from_slice(&fs::read(&destination)?)?;
        if existing.source_digest == record.source_digest {
            return Ok(Some(destination));
        }
        return Err("completed-session feedback changed after it was recorded".into());
    }
    atomic_json(&destination, &record)?;
    Ok(Some(destination))
}

#[must_use]
pub fn normalize_events(events: &[EventPayload]) -> Vec<TrajectoryEvent> {
    events
        .iter()
        .filter_map(|event| match event {
            EventPayload::ToolCallDenied { tool, reason } => Some(TrajectoryEvent {
                kind: "tool_denied".to_owned(),
                success: false,
                detail: format!("{tool}: {reason}"),
            }),
            EventPayload::ToolExecutionCompleted { tool, exit_code } => Some(TrajectoryEvent {
                kind: "tool".to_owned(),
                success: exit_code.is_none_or(|code| code == 0),
                detail: tool.clone(),
            }),
            EventPayload::VerificationCompleted { passed, evidence } => Some(TrajectoryEvent {
                kind: "verification".to_owned(),
                success: *passed,
                detail: if evidence.is_empty() {
                    "verification completed".to_owned()
                } else {
                    evidence.join(" | ")
                },
            }),
            EventPayload::SessionFailed { error } => Some(TrajectoryEvent {
                kind: "session_failure".to_owned(),
                success: false,
                detail: error.to_string(),
            }),
            EventPayload::SessionStateChanged { to, .. }
                if matches!(to, medusa_protocol::SessionState::Recovering) =>
            {
                Some(TrajectoryEvent {
                    kind: "retry".to_owned(),
                    success: true,
                    detail: "session entered recovery".to_owned(),
                })
            }
            _ => None,
        })
        .collect()
}

fn evaluate_session(input: &SessionFeedbackInput, trajectory: &TrajectoryAnalysis) -> CodingTaskOutcome {
    let verification_events = input
        .events
        .iter()
        .filter_map(|event| match event {
            EventPayload::VerificationCompleted { passed, .. } => Some(*passed),
            _ => None,
        })
        .collect::<Vec<_>>();
    let verified = !verification_events.is_empty() && verification_events.iter().all(|passed| *passed);
    let denied = input
        .events
        .iter()
        .filter(|event| matches!(event, EventPayload::ToolCallDenied { .. }))
        .count();
    let failed_tools = input
        .events
        .iter()
        .filter(|event| {
            matches!(
                event,
                EventPayload::ToolExecutionCompleted {
                    exit_code: Some(code),
                    ..
                } if *code != 0
            )
        })
        .count();

    let correctness = if verified { 1_000 } else if input.evidence_count > 0 { 700 } else { 250 };
    let safety = 1_000_u16.saturating_sub((denied as u16).saturating_mul(75));
    let efficiency = 1_000_u16.saturating_sub(
        input
            .turns
            .saturating_sub(3)
            .saturating_mul(35)
            .min(800) as u16,
    );
    let recovery = if trajectory.failures == 0 {
        1_000
    } else if trajectory.retries > 0 {
        750
    } else {
        350
    };
    let planning = if input.turns <= 8 { 900 } else { 650 };
    let maintainability = 1_000_u16.saturating_sub((failed_tools as u16).saturating_mul(60));

    CodingTaskOutcome {
        task_id: input.session_id.clone(),
        correctness_milli: correctness,
        scope_adherence_milli: 850,
        diff_quality_milli: 800,
        efficiency_milli: efficiency,
        safety_milli: safety,
        recovery_milli: recovery,
        planning_milli: planning,
        maintainability_milli: maintainability,
        user_burden_milli: if input.turns <= 5 { 950 } else { 750 },
        hidden_oracle_digest: source_digest(input).unwrap_or_else(|_| "0".repeat(64)),
        evidence: vec![format!(
            "{} durable evidence items; {} normalized trajectory events",
            input.evidence_count, trajectory.total_events
        )],
        metadata: [
            ("turns".to_owned(), input.turns.to_string()),
            ("verification_failures".to_owned(), trajectory.verification_failures.to_string()),
        ]
        .into_iter()
        .collect(),
    }
}

fn recommendations(
    trajectory: &TrajectoryAnalysis,
    evaluation: &CodingTaskOutcome,
) -> Vec<ImprovementRecommendation> {
    let mut result = Vec::new();
    if trajectory.verification_failures > 0 {
        result.push(ImprovementRecommendation {
            target: ImprovementTarget::TestDiscovery,
            risk: ImprovementRisk::Low,
            problem: "verification failed during the completed session".to_owned(),
            evidence: vec![format!(
                "{} verification failures were recorded",
                trajectory.verification_failures
            )],
            proposed_change: "improve repository-specific test discovery and run the narrowest relevant checks earlier".to_owned(),
            requires_human_review: false,
        });
    }
    if !trajectory.repeated_friction.is_empty() {
        result.push(ImprovementRecommendation {
            target: ImprovementTarget::CommandKnowledge,
            risk: ImprovementRisk::Low,
            problem: "the session repeated the same operational friction".to_owned(),
            evidence: trajectory.repeated_friction.clone(),
            proposed_change: "record the successful repository command sequence as provenance-backed command knowledge".to_owned(),
            requires_human_review: false,
        });
    }
    if evaluation.weighted_score_milli() < 750 {
        result.push(ImprovementRecommendation {
            target: ImprovementTarget::RecoveryHeuristic,
            risk: ImprovementRisk::Medium,
            problem: format!(
                "session evaluation score {} is below the promotion floor",
                evaluation.weighted_score_milli()
            ),
            evidence: evaluation.evidence.clone(),
            proposed_change: "review the failure trajectory and propose a bounded recovery-strategy update".to_owned(),
            requires_human_review: true,
        });
    }
    result
}

fn source_digest(input: &SessionFeedbackInput) -> Result<String, serde_json::Error> {
    let mut digest = Sha256::new();
    digest.update(serde_json::to_vec(input)?);
    Ok(format!("{:x}", digest.finalize()))
}

fn atomic_json(
    path: &Path,
    value: &impl Serialize,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_session_is_evaluated_and_recorded_once() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let input = SessionFeedbackInput {
            session_id: "session-1".to_owned(),
            objective: "repair the failing build".to_owned(),
            completed: true,
            turns: 4,
            evidence_count: 2,
            events: vec![EventPayload::VerificationCompleted {
                passed: true,
                evidence: vec!["cargo test passed".to_owned()],
            }],
        };

        let first = record_completed_session(directory.path(), &input)
            .expect("record feedback")
            .expect("feedback path");
        let second = record_completed_session(directory.path(), &input)
            .expect("record feedback again")
            .expect("feedback path");
        assert_eq!(first, second);

        let record: SessionFeedbackRecord =
            serde_json::from_slice(&fs::read(first).expect("read feedback"))
                .expect("feedback json");
        assert_eq!(record.evaluation.correctness_milli, 1_000);
        assert!(record.evaluation.weighted_score_milli() >= 900);
    }

    #[test]
    fn verification_failure_creates_test_discovery_recommendation() {
        let input = SessionFeedbackInput {
            session_id: "session-2".to_owned(),
            objective: "fix tests".to_owned(),
            completed: true,
            turns: 7,
            evidence_count: 1,
            events: vec![EventPayload::VerificationCompleted {
                passed: false,
                evidence: vec!["test failed".to_owned()],
            }],
        };
        let trajectory = analyze_trajectory(&normalize_events(&input.events));
        let evaluation = evaluate_session(&input, &trajectory);
        let recommendations = recommendations(&trajectory, &evaluation);
        assert!(recommendations.iter().any(|recommendation| {
            recommendation.target == ImprovementTarget::TestDiscovery
        }));
    }
}
