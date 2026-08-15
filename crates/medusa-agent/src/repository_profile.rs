use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use medusa_core::MedusaResult;
use serde::{Deserialize, Serialize};

const PROFILE_SCHEMA_VERSION: u16 = 1;
const MAX_PROFILE_AGE_MS: u128 = 90 * 24 * 60 * 60 * 1_000;
const MIN_CONFIDENCE: f64 = 0.55;
const MAX_SCORE_ADJUSTMENT: i64 = 40;
const MAX_OBSERVATIONS_PER_TOOL: u64 = 10_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LearnedOutputMode {
    Compact,
    Normal,
    Verbatim,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ToolObservation {
    pub successes: u64,
    pub failures: u64,
    pub average_latency_ms: u64,
    pub average_output_bytes: u64,
    pub recovery_reads: u64,
    pub preferred_output_mode: Option<LearnedOutputMode>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RepositoryProfile {
    pub schema_version: u16,
    pub enabled: bool,
    pub updated_unix_ms: u128,
    pub generation: u64,
    pub tools: BTreeMap<String, ToolObservation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ProfileDecision {
    pub status: String,
    pub score_adjustment: i64,
    pub confidence: f64,
    pub preferred_output_mode: Option<LearnedOutputMode>,
    pub reason: String,
}

impl Default for RepositoryProfile {
    fn default() -> Self {
        Self {
            schema_version: PROFILE_SCHEMA_VERSION,
            enabled: true,
            updated_unix_ms: now_ms(),
            generation: 0,
            tools: BTreeMap::new(),
        }
    }
}

pub(crate) fn decision(repo: &Path, tool: &str) -> ProfileDecision {
    let profile = match load(repo) {
        Ok(Some(profile)) => profile,
        Ok(None) => return ignored("profile is absent"),
        Err(_) => return ignored("profile is unreadable or invalid"),
    };
    if !profile.enabled {
        return ignored("profile is disabled");
    }
    if profile.schema_version != PROFILE_SCHEMA_VERSION {
        return ignored("profile schema is unsupported");
    }
    let age_ms = now_ms().saturating_sub(profile.updated_unix_ms);
    if age_ms > MAX_PROFILE_AGE_MS {
        return ignored("profile is stale");
    }
    let Some(observation) = profile.tools.get(tool) else {
        return ignored("tool has no learned observations");
    };
    let total = observation.successes.saturating_add(observation.failures);
    if total == 0 || total > MAX_OBSERVATIONS_PER_TOOL {
        return ignored("observation count is invalid");
    }
    let sample_confidence = (total as f64 / 12.0).min(1.0);
    let freshness = 1.0 - age_ms as f64 / MAX_PROFILE_AGE_MS as f64;
    let confidence = (sample_confidence * freshness).clamp(0.0, 1.0);
    if confidence < MIN_CONFIDENCE {
        return ignored("confidence is below the activation threshold");
    }
    let success_rate = observation.successes as f64 / total as f64;
    let recovery_penalty = (observation.recovery_reads.min(total) as f64 / total as f64) * 12.0;
    let raw = ((success_rate - 0.5) * 60.0 - recovery_penalty).round() as i64;
    let score_adjustment = raw.clamp(-MAX_SCORE_ADJUSTMENT, MAX_SCORE_ADJUSTMENT);
    ProfileDecision {
        status: "applied".into(),
        score_adjustment,
        confidence,
        preferred_output_mode: observation.preferred_output_mode,
        reason: format!(
            "bounded learned evidence from {total} observations; explicit policy and safety invariants retain precedence"
        ),
    }
}

pub(crate) fn record(
    repo: &Path,
    tool: &str,
    success: bool,
    latency_ms: u64,
    output_bytes: usize,
    output_mode: LearnedOutputMode,
    recovery_read: bool,
) -> MedusaResult<()> {
    let mut profile = load(repo)?.unwrap_or_default();
    if !profile.enabled || profile.schema_version != PROFILE_SCHEMA_VERSION {
        return Ok(());
    }
    let entry = profile
        .tools
        .entry(tool.to_owned())
        .or_insert(ToolObservation {
            successes: 0,
            failures: 0,
            average_latency_ms: latency_ms,
            average_output_bytes: output_bytes as u64,
            recovery_reads: 0,
            preferred_output_mode: None,
        });
    let previous = entry.successes.saturating_add(entry.failures);
    if previous >= MAX_OBSERVATIONS_PER_TOOL {
        return Ok(());
    }
    if success {
        entry.successes = entry.successes.saturating_add(1);
    } else {
        entry.failures = entry.failures.saturating_add(1);
    }
    if recovery_read {
        entry.recovery_reads = entry.recovery_reads.saturating_add(1);
    }
    let total = previous.saturating_add(1);
    entry.average_latency_ms =
        rolling_average(entry.average_latency_ms, previous, latency_ms, total);
    entry.average_output_bytes = rolling_average(
        entry.average_output_bytes,
        previous,
        output_bytes as u64,
        total,
    );
    entry.preferred_output_mode = Some(if recovery_read {
        LearnedOutputMode::Normal
    } else {
        output_mode
    });
    profile.updated_unix_ms = now_ms();
    profile.generation = profile.generation.saturating_add(1);
    persist(repo, &profile)
}

#[cfg(test)]
pub(crate) fn reset(repo: &Path) -> MedusaResult<()> {
    let path = profile_path(repo);
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

pub(crate) fn format_decision(decision: &ProfileDecision) -> String {
    format!(
        "[repository-profile status={}; score_adjustment={}; confidence={:.3}; preferred_output_mode={}; reason={}]",
        decision.status,
        decision.score_adjustment,
        decision.confidence,
        decision
            .preferred_output_mode
            .map(|mode| match mode {
                LearnedOutputMode::Compact => "compact",
                LearnedOutputMode::Normal => "normal",
                LearnedOutputMode::Verbatim => "verbatim",
            })
            .unwrap_or("none"),
        decision.reason
    )
}

fn ignored(reason: &str) -> ProfileDecision {
    ProfileDecision {
        status: "ignored".into(),
        score_adjustment: 0,
        confidence: 0.0,
        preferred_output_mode: None,
        reason: reason.into(),
    }
}

fn load(repo: &Path) -> MedusaResult<Option<RepositoryProfile>> {
    let path = profile_path(repo);
    if !path.exists() {
        return Ok(None);
    }
    let profile: RepositoryProfile = serde_json::from_slice(&fs::read(path)?)?;
    Ok(Some(profile))
}

fn persist(repo: &Path, profile: &RepositoryProfile) -> MedusaResult<()> {
    let path = profile_path(repo);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(profile)?)?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(temporary, path)?;
    Ok(())
}

fn profile_path(repo: &Path) -> PathBuf {
    repo.join(".medusa/orchestration-profile.json")
}

fn rolling_average(previous_average: u64, previous_count: u64, value: u64, total: u64) -> u64 {
    previous_average
        .saturating_mul(previous_count)
        .saturating_add(value)
        / total.max(1)
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_confidence_profile_does_not_change_policy() {
        let repo = tempfile::tempdir().expect("repository");
        record(
            repo.path(),
            "shell_run",
            true,
            10,
            20,
            LearnedOutputMode::Compact,
            false,
        )
        .expect("record");
        assert_eq!(decision(repo.path(), "shell_run").status, "ignored");
    }

    #[test]
    fn repeated_success_changes_recommendation_within_bound() {
        let repo = tempfile::tempdir().expect("repository");
        for _ in 0..12 {
            record(
                repo.path(),
                "shell_run",
                true,
                10,
                20,
                LearnedOutputMode::Compact,
                false,
            )
            .expect("record");
        }
        let learned = decision(repo.path(), "shell_run");
        assert_eq!(learned.status, "applied");
        assert!(learned.score_adjustment > 0);
        assert!(learned.score_adjustment <= MAX_SCORE_ADJUSTMENT);
    }

    #[test]
    fn corrupted_profile_fails_closed_and_reset_is_deterministic() {
        let repo = tempfile::tempdir().expect("repository");
        let path = profile_path(repo.path());
        fs::create_dir_all(path.parent().unwrap()).expect("profile directory");
        fs::write(&path, b"not-json").expect("corrupt profile");
        assert_eq!(decision(repo.path(), "shell_run").status, "ignored");
        reset(repo.path()).expect("reset");
        assert!(!path.exists());
    }
}
