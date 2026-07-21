use std::{collections::BTreeMap, fs, path::Path};

use medusa_core::MedusaResult;
use serde::{Deserialize, Serialize};

const ACTIVE_SKILLS_ROOT: &str = ".medusa/skills";
const METRICS_PATH: &str = ".medusa/learning/skill-metrics/summary.json";
const PROBATION_PATH: &str = ".medusa/learning/skill-probation/summary.json";
const LIFECYCLE_FILE: &str = "lifecycle.json";
const REQUIRED_POST_RESTORE_SAMPLES: usize = 3;
const HEALTHY_RATE_MILLI: u16 = 750;
const FAILURE_RATE_MILLI: u16 = 500;

#[derive(Debug, Deserialize)]
struct MetricsSummary {
    #[serde(default)]
    skills: BTreeMap<String, SkillMetric>,
}

#[derive(Clone, Debug, Deserialize)]
struct SkillMetric {
    observed_sessions: usize,
    verified_sessions: usize,
    verification_rate_milli: u16,
    confidence_milli: u16,
    latest_recorded_at: String,
}

#[derive(Debug, Deserialize)]
struct LifecycleRecord {
    skill: String,
    status: String,
    recommendation: BaselineRecommendation,
    restored_at_epoch_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct BaselineRecommendation {
    observed_sessions: usize,
    verification_rate_milli: u16,
    confidence_milli: u16,
}

#[derive(Debug, Serialize)]
struct ProbationSummary {
    schema_version: u8,
    policy: ProbationPolicy,
    skills: BTreeMap<String, ProbationReport>,
}

#[derive(Debug, Serialize)]
struct ProbationPolicy {
    required_post_restore_samples: usize,
    healthy_rate_milli: u16,
    failure_rate_milli: u16,
    automatic_quarantine: bool,
}

#[derive(Debug, Serialize)]
struct ProbationReport {
    state: String,
    baseline_observed_sessions: usize,
    baseline_verification_rate_milli: u16,
    baseline_confidence_milli: u16,
    post_restore_sessions: usize,
    post_restore_verified_sessions: usize,
    post_restore_verification_rate_milli: u16,
    post_restore_confidence_milli: u16,
    verification_rate_change_milli: i32,
    remaining_samples: usize,
    restored_at_epoch_seconds: Option<u64>,
    latest_recorded_at: String,
    recommendation: String,
}

pub(super) fn refresh(repo: &Path) -> MedusaResult<()> {
    let metrics = read_metrics(repo);
    let mut reports = BTreeMap::new();
    let root = repo.join(ACTIVE_SKILLS_ROOT);

    for lifecycle_path in lifecycle_paths(&root) {
        let Ok(bytes) = fs::read(&lifecycle_path) else {
            continue;
        };
        let Ok(lifecycle) = serde_json::from_slice::<LifecycleRecord>(&bytes) else {
            continue;
        };
        if lifecycle.status != "restored" || lifecycle.skill.trim().is_empty() {
            continue;
        }
        let Some(metric) = metrics.skills.get(&lifecycle.skill) else {
            reports.insert(
                lifecycle.skill.clone(),
                report_without_post_restore_evidence(&lifecycle),
            );
            continue;
        };
        reports.insert(lifecycle.skill.clone(), build_report(&lifecycle, metric));
    }

    let destination = repo.join(PROBATION_PATH);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    atomic_json(
        &destination,
        &ProbationSummary {
            schema_version: 1,
            policy: ProbationPolicy {
                required_post_restore_samples: REQUIRED_POST_RESTORE_SAMPLES,
                healthy_rate_milli: HEALTHY_RATE_MILLI,
                failure_rate_milli: FAILURE_RATE_MILLI,
                automatic_quarantine: false,
            },
            skills: reports,
        },
    )
}

fn read_metrics(repo: &Path) -> MetricsSummary {
    fs::read(repo.join(METRICS_PATH))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(MetricsSummary {
            skills: BTreeMap::new(),
        })
}

fn lifecycle_paths(root: &Path) -> Vec<std::path::PathBuf> {
    let mut paths = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path().join(LIFECYCLE_FILE))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn report_without_post_restore_evidence(lifecycle: &LifecycleRecord) -> ProbationReport {
    ProbationReport {
        state: "collecting".to_owned(),
        baseline_observed_sessions: lifecycle.recommendation.observed_sessions,
        baseline_verification_rate_milli: lifecycle.recommendation.verification_rate_milli,
        baseline_confidence_milli: lifecycle.recommendation.confidence_milli,
        post_restore_sessions: 0,
        post_restore_verified_sessions: 0,
        post_restore_verification_rate_milli: 0,
        post_restore_confidence_milli: calibrated_confidence_milli(0, 0),
        verification_rate_change_milli: 0,
        remaining_samples: REQUIRED_POST_RESTORE_SAMPLES,
        restored_at_epoch_seconds: lifecycle.restored_at_epoch_seconds,
        latest_recorded_at: String::new(),
        recommendation: "Collect post-restore evidence before changing trust.".to_owned(),
    }
}

fn build_report(lifecycle: &LifecycleRecord, metric: &SkillMetric) -> ProbationReport {
    let baseline_observed = lifecycle.recommendation.observed_sessions;
    let baseline_verified = approximate_verified_sessions(
        baseline_observed,
        lifecycle.recommendation.verification_rate_milli,
    );
    let post_observed = metric.observed_sessions.saturating_sub(baseline_observed);
    let post_verified = metric.verified_sessions.saturating_sub(baseline_verified);
    let post_rate = ratio_milli(post_verified, post_observed);
    let post_confidence = calibrated_confidence_milli(post_verified, post_observed);
    let remaining = REQUIRED_POST_RESTORE_SAMPLES.saturating_sub(post_observed);
    let (state, recommendation) = probation_state(
        post_observed,
        post_rate,
        post_confidence,
        lifecycle.recommendation.confidence_milli,
    );

    ProbationReport {
        state: state.to_owned(),
        baseline_observed_sessions: baseline_observed,
        baseline_verification_rate_milli: lifecycle.recommendation.verification_rate_milli,
        baseline_confidence_milli: lifecycle.recommendation.confidence_milli,
        post_restore_sessions: post_observed,
        post_restore_verified_sessions: post_verified,
        post_restore_verification_rate_milli: post_rate,
        post_restore_confidence_milli: post_confidence,
        verification_rate_change_milli: i32::from(post_rate)
            - i32::from(lifecycle.recommendation.verification_rate_milli),
        remaining_samples: remaining,
        restored_at_epoch_seconds: lifecycle.restored_at_epoch_seconds,
        latest_recorded_at: metric.latest_recorded_at.clone(),
        recommendation: recommendation.to_owned(),
    }
}

fn probation_state(
    post_observed: usize,
    post_rate: u16,
    post_confidence: u16,
    baseline_confidence: u16,
) -> (&'static str, &'static str) {
    if post_observed < REQUIRED_POST_RESTORE_SAMPLES {
        return (
            "collecting",
            "Collect more post-restore sessions before making a lifecycle decision.",
        );
    }
    if post_rate < FAILURE_RATE_MILLI {
        return (
            "failed",
            "Review the restored skill and consider explicit re-quarantine.",
        );
    }
    if post_rate >= HEALTHY_RATE_MILLI && post_confidence >= baseline_confidence {
        return (
            "passed",
            "The restored skill has recovered enough evidence to remain active.",
        );
    }
    (
        "watch",
        "Keep the skill active under observation and collect more evidence.",
    )
}

fn approximate_verified_sessions(observed: usize, rate_milli: u16) -> usize {
    observed
        .saturating_mul(usize::from(rate_milli))
        .saturating_add(500)
        / 1_000
}

fn ratio_milli(numerator: usize, denominator: usize) -> u16 {
    if denominator == 0 {
        0
    } else {
        ((numerator.saturating_mul(1_000)) / denominator) as u16
    }
}

fn calibrated_confidence_milli(verified: usize, observed: usize) -> u16 {
    ratio_milli(verified.saturating_add(2), observed.saturating_add(4))
}

fn atomic_json(path: &Path, value: &impl Serialize) -> MedusaResult<()> {
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restored_skill(repo: &Path, name: &str, observed: usize, rate: u16, confidence: u16) {
        let directory = repo.join(ACTIVE_SKILLS_ROOT).join(name);
        fs::create_dir_all(&directory).expect("skill directory");
        fs::write(directory.join("SKILL.md"), "# Restored skill\n").expect("skill");
        fs::write(
            directory.join(LIFECYCLE_FILE),
            serde_json::to_vec_pretty(&serde_json::json!({
                "skill": name,
                "status": "restored",
                "recommendation": {
                    "observed_sessions": observed,
                    "verification_rate_milli": rate,
                    "confidence_milli": confidence
                },
                "restored_at_epoch_seconds": 100
            }))
            .expect("lifecycle json"),
        )
        .expect("lifecycle");
    }

    fn metrics(repo: &Path, name: &str, observed: usize, verified: usize) {
        let path = repo.join(METRICS_PATH);
        fs::create_dir_all(path.parent().expect("metrics parent")).expect("metrics directory");
        fs::write(
            path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "skills": {
                    name: {
                        "observed_sessions": observed,
                        "verified_sessions": verified,
                        "verification_rate_milli": ratio_milli(verified, observed),
                        "confidence_milli": calibrated_confidence_milli(verified, observed),
                        "latest_recorded_at": "2026-07-21T16:00:00Z"
                    }
                }
            }))
            .expect("metrics json"),
        )
        .expect("metrics");
    }

    #[test]
    fn restored_skill_collects_three_post_restore_samples() {
        let repo = tempfile::tempdir().expect("repo");
        restored_skill(repo.path(), "verify", 5, 200, 333);
        metrics(repo.path(), "verify", 6, 2);
        refresh(repo.path()).expect("refresh probation");
        let summary: serde_json::Value = serde_json::from_slice(
            &fs::read(repo.path().join(PROBATION_PATH)).expect("probation summary"),
        )
        .expect("summary json");
        assert_eq!(summary["skills"]["verify"]["state"], "collecting");
        assert_eq!(summary["skills"]["verify"]["remaining_samples"], 2);
    }

    #[test]
    fn recovered_skill_passes_and_regressed_skill_fails() {
        let recovered = tempfile::tempdir().expect("recovered repo");
        restored_skill(recovered.path(), "verify", 5, 200, 333);
        metrics(recovered.path(), "verify", 8, 4);
        refresh(recovered.path()).expect("refresh recovered");
        let recovered_summary: serde_json::Value = serde_json::from_slice(
            &fs::read(recovered.path().join(PROBATION_PATH)).expect("recovered summary"),
        )
        .expect("recovered json");
        assert_eq!(recovered_summary["skills"]["verify"]["state"], "passed");

        let regressed = tempfile::tempdir().expect("regressed repo");
        restored_skill(regressed.path(), "verify", 5, 200, 333);
        metrics(regressed.path(), "verify", 8, 1);
        refresh(regressed.path()).expect("refresh regressed");
        let regressed_summary: serde_json::Value = serde_json::from_slice(
            &fs::read(regressed.path().join(PROBATION_PATH)).expect("regressed summary"),
        )
        .expect("regressed json");
        assert_eq!(regressed_summary["skills"]["verify"]["state"], "failed");
    }

    #[test]
    fn non_restored_lifecycle_records_are_ignored() {
        let repo = tempfile::tempdir().expect("repo");
        restored_skill(repo.path(), "verify", 5, 200, 333);
        let lifecycle = repo
            .path()
            .join(ACTIVE_SKILLS_ROOT)
            .join("verify")
            .join(LIFECYCLE_FILE);
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&lifecycle).expect("lifecycle")).expect("json");
        value["status"] = serde_json::json!("quarantined");
        fs::write(lifecycle, serde_json::to_vec_pretty(&value).expect("json")).expect("write");
        refresh(repo.path()).expect("refresh");
        let summary: serde_json::Value =
            serde_json::from_slice(&fs::read(repo.path().join(PROBATION_PATH)).expect("summary"))
                .expect("summary json");
        assert_eq!(summary["skills"], serde_json::json!({}));
    }
}
