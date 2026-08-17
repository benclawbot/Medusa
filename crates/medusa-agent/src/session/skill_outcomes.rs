use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::{AgentSession, secure_state};
use crate::tools::skills::automatically_loaded_names;
use medusa_failure::FailureDecision;

#[cfg(test)]
use std::collections::BTreeMap;

#[cfg(test)]
const ACTIVE_SKILLS_ROOT: &str = ".medusa/skills";
const SESSION_SKILLS_ROOT: &str = ".medusa/learning/session-skills";
const OUTCOME_ROOT: &str = ".medusa/learning/skill-outcomes";
#[cfg(test)]
const METRICS_PATH: &str = ".medusa/learning/skill-metrics/summary.json";
#[cfg(test)]
const REVIEW_PATH: &str = ".medusa/learning/skill-reviews/recommendations.json";
#[cfg(test)]
const MIN_REVIEW_SAMPLES: usize = 5;
#[cfg(test)]
const HEALTHY_RATE_MILLI: u16 = 750;
#[cfg(test)]
const REVIEW_RATE_MILLI: u16 = 500;

#[derive(Debug, Deserialize, Serialize)]
struct SkillOutcomeRecord {
    schema_version: u8,
    session_id: String,
    objective: String,
    recorded_at: String,
    completed: bool,
    verified: bool,
    turns: u32,
    evidence_count: usize,
    automatically_loaded_skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terminal_failure: Option<TerminalFailure>,
}

#[derive(Debug, Deserialize, Serialize)]
struct TerminalFailure {
    code: String,
    category: String,
    disposition: String,
    reason: String,
}

#[cfg(test)]
#[derive(Debug, Default, Eq, PartialEq, Serialize)]
struct SkillEffectivenessMetric {
    observed_sessions: usize,
    verified_sessions: usize,
    verification_rate_milli: u16,
    confidence_milli: u16,
    evidence_state: String,
    review_recommended: bool,
    recommendation_reason: Option<String>,
    average_turns_milli: u64,
    average_evidence_milli: u64,
    latest_recorded_at: String,
}

#[cfg(test)]
#[derive(Debug, Serialize)]
struct SkillEffectivenessSummary {
    schema_version: u8,
    policy: ConfidencePolicy,
    skills: BTreeMap<String, SkillEffectivenessMetric>,
}

#[cfg(test)]
#[derive(Debug, Serialize)]
struct ConfidencePolicy {
    minimum_review_samples: usize,
    healthy_rate_milli: u16,
    review_rate_milli: u16,
    prior_verified: usize,
    prior_observed: usize,
}

#[cfg(test)]
#[derive(Debug, Serialize)]
struct SkillReviewSummary {
    schema_version: u8,
    recommendations: Vec<SkillReviewRecommendation>,
}

#[cfg(test)]
#[derive(Debug, Serialize)]
struct SkillReviewRecommendation {
    skill: String,
    observed_sessions: usize,
    verification_rate_milli: u16,
    confidence_milli: u16,
    reason: String,
    latest_recorded_at: String,
}

#[cfg(test)]
#[derive(Default)]
struct MetricAccumulator {
    observed_sessions: usize,
    verified_sessions: usize,
    turns: u64,
    evidence_count: u64,
    latest_recorded_at: String,
}

/// Retains the legacy loaded-skill inventory for compatibility. Production causal attribution is
/// intentionally owned by `LearningMonitorStore`; this helper is not called by session completion.
#[cfg(test)]
pub(super) fn record_completed_session(session: &AgentSession) -> MedusaResult<Option<PathBuf>> {
    if !session.completed {
        return Ok(None);
    }

    let skills = loaded_skill_names(session);
    if skills.is_empty() {
        return Ok(None);
    }

    let root = session.repo.join(OUTCOME_ROOT);
    secure_state::create_dir_all(&root)?;
    let destination = root.join(format!("{}.json", session.id));
    if !destination.is_file() {
        let recorded_at = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|error| {
                MedusaError::new(
                    ErrorCode::InternalInvariant,
                    ErrorCategory::Internal,
                    format!("could not format skill outcome timestamp: {error}"),
                )
            })?;
        let record = SkillOutcomeRecord {
            schema_version: 1,
            session_id: session.id.to_string(),
            objective: session.objective.clone(),
            recorded_at,
            completed: true,
            verified: verification_passed(session),
            turns: session.turn,
            evidence_count: session.evidence.len(),
            automatically_loaded_skills: skills,
            terminal_failure: None,
        };
        atomic_json(&destination, &record)?;
    }
    rebuild_effectiveness_summary(&session.repo)?;
    Ok(Some(destination))
}

pub(crate) fn record_loaded_skills(session: &AgentSession) -> MedusaResult<()> {
    let mut loaded = loaded_skill_names(session);
    loaded.extend(automatically_loaded_names(&session.repo));
    loaded.sort();
    loaded.dedup();
    if loaded.is_empty() {
        return Ok(());
    }
    let path = session
        .repo
        .join(SESSION_SKILLS_ROOT)
        .join(format!("{}.json", session.id));
    if let Some(parent) = path.parent() {
        secure_state::create_dir_all(parent)?;
    }
    atomic_json(&path, &loaded)
}

fn loaded_skill_names(session: &AgentSession) -> Vec<String> {
    fs::read(
        session
            .repo
            .join(SESSION_SKILLS_ROOT)
            .join(format!("{}.json", session.id)),
    )
    .ok()
    .and_then(|content| serde_json::from_slice(&content).ok())
    .unwrap_or_default()
}

#[cfg(test)]
pub(super) fn verification_passed(session: &AgentSession) -> bool {
    session
        .events
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            medusa_protocol::EventPayload::VerificationCompleted { passed, .. } => Some(*passed),
            _ => None,
        })
        == Some(true)
}

pub(crate) fn record_terminal_skill_outcome(
    session: &AgentSession,
    error: &MedusaError,
    decision: &FailureDecision,
    reason: &str,
) -> MedusaResult<Option<PathBuf>> {
    let skills = loaded_skill_names(session);
    if skills.is_empty() {
        return Ok(None);
    }
    let root = session.repo.join(OUTCOME_ROOT);
    secure_state::create_dir_all(&root)?;
    let destination = root.join(format!("{}.json", session.id));
    let recorded_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|format_error| {
            MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                format!("could not format skill outcome timestamp: {format_error}"),
            )
        })?;
    let record = SkillOutcomeRecord {
        schema_version: 2,
        session_id: session.id.to_string(),
        objective: session.objective.clone(),
        recorded_at,
        completed: false,
        verified: false,
        turns: session.turn,
        evidence_count: session.evidence.len(),
        automatically_loaded_skills: skills,
        terminal_failure: Some(TerminalFailure {
            code: error.code.to_string(),
            category: format!("{:?}", error.category).to_ascii_lowercase(),
            disposition: format!("{:?}", decision.disposition).to_ascii_lowercase(),
            reason: reason.to_owned(),
        }),
    };
    atomic_json(&destination, &record)?;
    // Terminal skill inventories are retained for audit only. Causal effectiveness is recorded
    // by `LearningMonitorStore`, which links applied exposures to authoritative outcomes.
    Ok(Some(destination))
}

#[cfg(test)]
fn rebuild_effectiveness_summary(repo: &Path) -> MedusaResult<PathBuf> {
    let mut accumulators = BTreeMap::<String, MetricAccumulator>::new();
    for record in outcome_records(repo) {
        for skill in record.automatically_loaded_skills {
            let metric = accumulators.entry(skill).or_default();
            metric.observed_sessions += 1;
            metric.verified_sessions += usize::from(record.verified);
            metric.turns = metric.turns.saturating_add(u64::from(record.turns));
            metric.evidence_count = metric
                .evidence_count
                .saturating_add(record.evidence_count as u64);
            if record.recorded_at > metric.latest_recorded_at {
                metric.latest_recorded_at = record.recorded_at.clone();
            }
        }
    }

    let mut recommendations = Vec::new();
    let skills = accumulators
        .into_iter()
        .map(|(name, metric)| {
            let samples = metric.observed_sessions as u64;
            let verification_rate_milli =
                ratio_milli(metric.verified_sessions, metric.observed_sessions);
            let confidence_milli =
                calibrated_confidence_milli(metric.verified_sessions, metric.observed_sessions);
            let (evidence_state, reason) = evidence_state(
                metric.observed_sessions,
                verification_rate_milli,
                confidence_milli,
            );
            let review_recommended = reason.is_some();
            if let Some(reason) = &reason {
                recommendations.push(SkillReviewRecommendation {
                    skill: name.clone(),
                    observed_sessions: metric.observed_sessions,
                    verification_rate_milli,
                    confidence_milli,
                    reason: reason.clone(),
                    latest_recorded_at: metric.latest_recorded_at.clone(),
                });
            }
            let effectiveness = SkillEffectivenessMetric {
                observed_sessions: metric.observed_sessions,
                verified_sessions: metric.verified_sessions,
                verification_rate_milli,
                confidence_milli,
                evidence_state: evidence_state.to_owned(),
                review_recommended,
                recommendation_reason: reason,
                average_turns_milli: average_milli(metric.turns, samples),
                average_evidence_milli: average_milli(metric.evidence_count, samples),
                latest_recorded_at: metric.latest_recorded_at,
            };
            (name, effectiveness)
        })
        .collect();

    let summary = SkillEffectivenessSummary {
        schema_version: 2,
        policy: ConfidencePolicy {
            minimum_review_samples: MIN_REVIEW_SAMPLES,
            healthy_rate_milli: HEALTHY_RATE_MILLI,
            review_rate_milli: REVIEW_RATE_MILLI,
            prior_verified: 2,
            prior_observed: 4,
        },
        skills,
    };
    let destination = repo.join(METRICS_PATH);
    if let Some(parent) = destination.parent() {
        secure_state::create_dir_all(parent)?;
    }
    atomic_json(&destination, &summary)?;
    write_review_recommendations(repo, recommendations)?;
    Ok(destination)
}

#[cfg(test)]
fn write_review_recommendations(
    repo: &Path,
    mut recommendations: Vec<SkillReviewRecommendation>,
) -> MedusaResult<()> {
    recommendations.sort_by(|left, right| left.skill.cmp(&right.skill));
    let destination = repo.join(REVIEW_PATH);
    if let Some(parent) = destination.parent() {
        secure_state::create_dir_all(parent)?;
    }
    atomic_json(
        &destination,
        &SkillReviewSummary {
            schema_version: 1,
            recommendations,
        },
    )
}

#[cfg(test)]
fn evidence_state(
    observed_sessions: usize,
    verification_rate_milli: u16,
    confidence_milli: u16,
) -> (&'static str, Option<String>) {
    if observed_sessions < MIN_REVIEW_SAMPLES {
        return ("collecting", None);
    }
    if verification_rate_milli < REVIEW_RATE_MILLI {
        return (
            "review",
            Some(format!(
                "verification rate is {:.1}% after {observed_sessions} observations (calibrated confidence {:.1}%)",
                f64::from(verification_rate_milli) / 10.0,
                f64::from(confidence_milli) / 10.0
            )),
        );
    }
    if verification_rate_milli < HEALTHY_RATE_MILLI {
        ("watch", None)
    } else {
        ("healthy", None)
    }
}

#[cfg(test)]
fn calibrated_confidence_milli(verified: usize, observed: usize) -> u16 {
    ratio_milli(verified.saturating_add(2), observed.saturating_add(4))
}

#[cfg(test)]
fn outcome_records(repo: &Path) -> Vec<SkillOutcomeRecord> {
    let mut paths = fs::read_dir(repo.join(OUTCOME_ROOT))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| fs::read(path).ok())
        .filter_map(|content| serde_json::from_slice(&content).ok())
        .collect()
}

#[cfg(test)]
fn ratio_milli(numerator: usize, denominator: usize) -> u16 {
    if denominator == 0 {
        0
    } else {
        ((numerator.saturating_mul(1_000)) / denominator) as u16
    }
}

#[cfg(test)]
fn average_milli(total: u64, samples: u64) -> u64 {
    if samples == 0 {
        0
    } else {
        total.saturating_mul(1_000) / samples
    }
}

fn atomic_json(path: &Path, value: &impl Serialize) -> MedusaResult<()> {
    if let Some(parent) = path.parent() {
        secure_state::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    let result = (|| {
        let mut file = secure_state::create_new_file(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(value)?)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        secure_state::repair(path, false)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use medusa_core::SessionId;
    use medusa_protocol::{Actor, EventPayload};
    use time::OffsetDateTime;

    use crate::evidence::append_event;

    use super::*;

    fn session(repo: PathBuf, completed: bool) -> AgentSession {
        let mut session = AgentSession {
            id: SessionId::new(),
            objective: "verify the release".to_owned(),
            repo,
            created_at: OffsetDateTime::now_utc(),
            updated_at: OffsetDateTime::now_utc(),
            completed,
            turn: 3,
            plan: Vec::new(),
            pending_question: None,
            messages: Vec::new(),
            events: Vec::new(),
            evidence: vec!["cargo test passed".to_owned()],
            tool_artifacts: Vec::new(),
            approval_grants: Vec::new(),
            approval_receipts: Vec::new(),
            rollback_receipts: Vec::new(),
            world_model: None,
        };
        if completed {
            append_event(
                &mut session,
                Actor::System("test".to_owned()),
                EventPayload::VerificationCompleted {
                    passed: true,
                    evidence: vec!["cargo test passed".to_owned()],
                },
            )
            .expect("verification event");
        }
        session
    }

    fn install_skills(repo: &Path) {
        for name in ["release", "verify"] {
            let skill = repo.join(ACTIVE_SKILLS_ROOT).join(name).join("SKILL.md");
            fs::create_dir_all(skill.parent().expect("skill parent")).expect("create skill");
            fs::write(skill, "# Approved skill\n").expect("write skill");
        }
    }

    #[test]
    fn completed_session_records_loaded_skills_once() {
        let directory = tempfile::tempdir().expect("temporary directory");
        install_skills(directory.path());
        let session = session(directory.path().to_path_buf(), true);
        record_loaded_skills(&session).expect("record loaded skills");

        let first = record_completed_session(&session)
            .expect("record outcome")
            .expect("outcome path");
        let second = record_completed_session(&session)
            .expect("record outcome again")
            .expect("outcome path");

        assert_eq!(first, second);
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(first).expect("read outcome")).expect("outcome json");
        assert_eq!(value["completed"], true);
        assert_eq!(value["verified"], true);
        assert_eq!(
            value["automatically_loaded_skills"],
            serde_json::json!(["release", "verify"])
        );
    }

    #[test]
    fn outcome_uses_skills_recorded_when_request_was_built() {
        let directory = tempfile::tempdir().expect("temporary directory");
        install_skills(directory.path());
        let session = session(directory.path().to_path_buf(), true);
        record_loaded_skills(&session).expect("record loaded skills");
        fs::remove_dir_all(directory.path().join(ACTIVE_SKILLS_ROOT).join("release"))
            .expect("remove loaded skill");
        let late = directory
            .path()
            .join(ACTIVE_SKILLS_ROOT)
            .join("late")
            .join("SKILL.md");
        fs::create_dir_all(late.parent().expect("late parent")).expect("create late skill");
        fs::write(late, "# Added after request\n").expect("write late skill");

        let outcome = record_completed_session(&session)
            .expect("record outcome")
            .expect("outcome path");
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(outcome).expect("read outcome"))
                .expect("outcome json");
        assert_eq!(
            value["automatically_loaded_skills"],
            serde_json::json!(["release", "verify"])
        );
    }

    #[test]
    fn early_metrics_collect_evidence_without_recommending_review() {
        let directory = tempfile::tempdir().expect("temporary directory");
        install_skills(directory.path());
        let session = session(directory.path().to_path_buf(), true);
        record_loaded_skills(&session).expect("record loaded skills");
        record_completed_session(&session).expect("outcome");

        let summary: serde_json::Value = serde_json::from_slice(
            &fs::read(directory.path().join(METRICS_PATH)).expect("read metrics"),
        )
        .expect("metrics json");
        let release = &summary["skills"]["release"];
        assert_eq!(summary["schema_version"], 2);
        assert_eq!(release["evidence_state"], "collecting");
        assert_eq!(release["review_recommended"], false);
        assert_eq!(release["confidence_milli"], 600);
    }

    #[test]
    fn repeated_unverified_outcomes_create_durable_review_recommendation() {
        let directory = tempfile::tempdir().expect("temporary directory");
        install_skills(directory.path());
        for _ in 0..5 {
            let mut failed = session(directory.path().to_path_buf(), true);
            append_event(
                &mut failed,
                Actor::System("test".to_owned()),
                EventPayload::VerificationCompleted {
                    passed: false,
                    evidence: vec!["cargo test failed".to_owned()],
                },
            )
            .expect("failed verification event");
            record_loaded_skills(&failed).expect("record loaded skills");
            record_completed_session(&failed).expect("outcome");
        }

        let summary: serde_json::Value = serde_json::from_slice(
            &fs::read(directory.path().join(METRICS_PATH)).expect("read metrics"),
        )
        .expect("metrics json");
        let release = &summary["skills"]["release"];
        assert_eq!(release["observed_sessions"], 5);
        assert_eq!(release["evidence_state"], "review");
        assert_eq!(release["review_recommended"], true);

        let reviews: serde_json::Value = serde_json::from_slice(
            &fs::read(directory.path().join(REVIEW_PATH)).expect("read reviews"),
        )
        .expect("review json");
        assert_eq!(reviews["recommendations"][0]["skill"], "release");
    }

    #[test]
    fn confidence_calibration_uses_stable_prior() {
        assert_eq!(calibrated_confidence_milli(0, 0), 500);
        assert_eq!(calibrated_confidence_milli(1, 1), 600);
        assert_eq!(calibrated_confidence_milli(5, 5), 777);
    }

    #[test]
    fn incomplete_session_does_not_record_an_outcome() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let session = session(directory.path().to_path_buf(), false);
        assert_eq!(record_completed_session(&session).expect("record"), None);
    }
}
