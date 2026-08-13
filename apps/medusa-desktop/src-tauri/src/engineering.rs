//! Read-only desktop projection over the shared learning authority.
//!
//! This module deliberately owns no improvement lifecycle state. It consumes typed provenance,
//! the #822 monitor, and the #823 meta-improvement store, while legacy dashboard records are
//! imported once as explicitly untrusted compatibility data.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use medusa_improvement::{
    learning_monitor::{LearningMonitorStore, OutcomeRecord, OutcomeStatus},
    meta_improvement::{MetaImprovementStatus, MetaImprovementStore},
    provenance::{ProvenanceGraphStore, ProvenanceOutcome, ProvenanceSource},
};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineeringPoint {
    pub date: String,
    pub total: u32,
    pub successful: u32,
    pub success_rate: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrictionItem {
    pub category: String,
    pub count: u32,
    pub sessions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImprovementApproval {
    pub reviewer: String,
    pub approved_at: String,
    pub proposal_revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImprovementObservation {
    pub observed_at: String,
    pub trigger_count: u32,
    pub correction_count: u32,
    pub regression_count: u32,
    pub latency_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImprovementRecord {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub title: String,
    pub problem: String,
    pub proposed_change: String,
    pub evidence: Vec<String>,
    pub source_sessions: Vec<String>,
    pub risk: String,
    pub status: String,
    pub benchmark_before: Option<f64>,
    pub benchmark_after: Option<f64>,
    pub rollback_note: String,
    #[serde(default = "default_revision")]
    pub revision: u64,
    #[serde(default)]
    pub approval: Option<ImprovementApproval>,
    #[serde(default)]
    pub active_version: Option<String>,
    #[serde(default)]
    pub previous_version: Option<String>,
    #[serde(default)]
    pub conflicts_with: BTreeSet<String>,
    #[serde(default)]
    pub observations: Vec<ImprovementObservation>,
    #[serde(default)]
    pub suspension_reason: Option<String>,
}

const fn default_revision() -> u64 {
    1
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineeringDashboard {
    pub total_tasks: u32,
    pub successful_tasks: u32,
    pub success_rate: f64,
    pub verification_pass_rate: f64,
    pub average_retries: f64,
    pub human_intervention_rate: f64,
    pub rollback_rate: f64,
    pub average_duration_minutes: f64,
    pub trend: Vec<EngineeringPoint>,
    pub friction: Vec<FrictionItem>,
    pub improvements: Vec<ImprovementRecord>,
    pub generated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyMigrationReceipt {
    schema_version: u32,
    source_path: String,
    source_digest: String,
    imported_at: String,
    records: Vec<ImprovementRecord>,
    error: Option<String>,
}

const LEGACY_SCHEMA_VERSION: u32 = 1;

#[tauri::command]
pub fn runtime_engineering_dashboard(
    repo: String,
    days: Option<u32>,
) -> Result<EngineeringDashboard, String> {
    let repo = canonical_repo(&repo)?;
    let cutoff = OffsetDateTime::now_utc() - Duration::days(i64::from(days.unwrap_or(90)));
    let cutoff_unix_ms = cutoff.unix_timestamp_nanos() as i64 / 1_000_000;

    let monitor = LearningMonitorStore::open(&repo).map_err(|error| error.to_string())?;
    let monitor_snapshot = monitor.snapshot();
    let mut outcomes = BTreeMap::<String, OutcomeRecord>::new();
    for outcome in monitor_snapshot.unattributed_outcomes {
        outcomes.insert(outcome.id.clone(), outcome);
    }
    for artifact in monitor_snapshot.artifacts {
        for outcome in artifact.outcomes {
            outcomes.insert(outcome.id.clone(), outcome);
        }
    }

    let mut by_day = BTreeMap::<String, (u32, u32)>::new();
    let mut total = 0;
    let mut successful = 0;
    let mut verification_total = 0;
    let mut verification_passed = 0;
    let mut retries = 0;
    let mut interventions = 0;
    let mut latency_millis = 0_u64;
    for outcome in outcomes
        .values()
        .filter(|outcome| outcome.recorded_at_unix_ms >= cutoff_unix_ms)
    {
        total += 1;
        if outcome.status == OutcomeStatus::Positive {
            successful += 1;
        }
        let date = unix_ms_to_date(outcome.recorded_at_unix_ms);
        let entry = by_day.entry(date).or_default();
        entry.0 += 1;
        if outcome.status == OutcomeStatus::Positive {
            entry.1 += 1;
        }
        retries += outcome.retries;
        latency_millis = latency_millis.saturating_add(outcome.latency_millis);
        if outcome.user_correction_count > 0 || outcome.parent_review_revisions > 0 {
            interventions += 1;
        }
        if let Some(passed) = outcome.verification_passed {
            verification_total += 1;
            verification_passed += u32::from(passed);
        }
    }

    let provenance = ProvenanceGraphStore::open(&repo).map_err(|error| error.to_string())?;
    let mut friction = BTreeMap::<String, (u32, Vec<String>)>::new();
    for observation in provenance
        .graph()
        .observations
        .iter()
        .filter(|observation| {
            observation.observed_at.unix_timestamp_nanos() as i64 / 1_000_000 >= cutoff_unix_ms
        })
    {
        let category = match observation.source {
            ProvenanceSource::UserCorrection => Some("user corrections"),
            ProvenanceSource::ParentReview => Some("parent-review revisions"),
            ProvenanceSource::ToolExecution | ProvenanceSource::ToolTelemetry
                if observation.outcome != ProvenanceOutcome::Positive =>
            {
                Some("tool failures")
            }
            ProvenanceSource::Verification
                if observation.outcome != ProvenanceOutcome::Positive =>
            {
                Some("verification failures")
            }
            ProvenanceSource::RuntimeFailure => Some("runtime failures"),
            ProvenanceSource::Recovery if observation.outcome != ProvenanceOutcome::Positive => {
                Some("recovery failures")
            }
            ProvenanceSource::Integration if observation.outcome != ProvenanceOutcome::Positive => {
                Some("integration failures")
            }
            _ => None,
        };
        if let Some(category) = category {
            add_friction(&mut friction, category, &observation.session_id);
        }
    }
    let mut friction = friction
        .into_iter()
        .map(|(category, (count, sessions))| FrictionItem {
            category,
            count,
            sessions,
        })
        .collect::<Vec<_>>();
    friction.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.category.cmp(&right.category))
    });

    let improvements = read_improvements(&repo)?;
    let rolled_back = improvements
        .iter()
        .filter(|item| item.status == "rolledBack")
        .count() as u32;
    let trend = by_day
        .into_iter()
        .map(|(date, (total, successful))| EngineeringPoint {
            date,
            total,
            successful,
            success_rate: rate(successful, total),
        })
        .collect();
    Ok(EngineeringDashboard {
        total_tasks: total,
        successful_tasks: successful,
        success_rate: rate(successful, total),
        verification_pass_rate: rate(verification_passed, verification_total),
        average_retries: if total == 0 {
            0.0
        } else {
            retries as f64 / total as f64
        },
        human_intervention_rate: rate(interventions, total),
        rollback_rate: rate(rolled_back, improvements.len() as u32),
        average_duration_minutes: average_duration_minutes(latency_millis, total),
        trend,
        friction,
        improvements,
        generated_at: OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_default(),
    })
}

fn read_improvements(repo: &Path) -> Result<Vec<ImprovementRecord>, String> {
    let store = MetaImprovementStore::open(repo).map_err(|error| error.to_string())?;
    let mut records = store
        .snapshot()
        .proposals
        .into_iter()
        .map(meta_record)
        .collect::<Vec<_>>();
    records.extend(read_legacy_compatibility(repo)?);
    records.sort_by(|left, right| {
        left.updated_at
            .cmp(&right.updated_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(records)
}

fn meta_record(
    proposal: medusa_improvement::meta_improvement::MetaImprovementProposal,
) -> ImprovementRecord {
    let status = match proposal.status {
        MetaImprovementStatus::Proposed => "proposed",
        MetaImprovementStatus::AwaitingReview => "awaitingReview",
        MetaImprovementStatus::Approved => "approved",
        MetaImprovementStatus::Activated => "active",
        MetaImprovementStatus::RolledBack => "rolledBack",
        MetaImprovementStatus::Rejected => "rejected",
        MetaImprovementStatus::Escalated => "escalated",
    };
    let timestamp = unix_ms_to_timestamp(proposal.created_at_unix_ms);
    ImprovementRecord {
        id: proposal.id.clone(),
        created_at: timestamp.clone(),
        updated_at: timestamp,
        title: format!("{:?} meta-improvement", proposal.target),
        problem: proposal.current_behavior,
        proposed_change: proposal.minimal_change,
        evidence: proposal.source_signal_ids,
        source_sessions: proposal.source_trajectory_ids,
        risk: if proposal.security_impact || proposal.capability_impact {
            "engineering review"
        } else {
            "bounded runtime"
        }
        .into(),
        status: status.into(),
        benchmark_before: None,
        benchmark_after: None,
        rollback_note: proposal.exact_rollback,
        revision: 1,
        approval: None,
        active_version: (proposal.status == MetaImprovementStatus::Activated)
            .then(|| proposal.id.clone()),
        previous_version: None,
        conflicts_with: BTreeSet::new(),
        observations: Vec::new(),
        suspension_reason: None,
    }
}

fn read_legacy_compatibility(repo: &Path) -> Result<Vec<ImprovementRecord>, String> {
    let root = repo.join(".medusa/engineering");
    let receipt_path = root.join("migration-receipt.json");
    let legacy = root.join("improvements.json");
    if receipt_path.is_file() {
        let receipt: LegacyMigrationReceipt =
            serde_json::from_slice(&fs::read(&receipt_path).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        quarantine_legacy_if_present(&root, &legacy, &receipt.source_digest)?;
        return Ok(receipt.records);
    }
    if !legacy.is_file() {
        return Ok(Vec::new());
    }
    let bytes = fs::read(&legacy).map_err(|error| error.to_string())?;
    let digest = repository_key_bytes(&bytes);
    let imported_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default();
    let parsed = serde_json::from_slice::<Vec<ImprovementRecord>>(&bytes);
    let (records, error) = match parsed {
        Ok(items) => (
            items
                .into_iter()
                .map(|mut item| {
                    item.status = "legacyUntrusted".into();
                    item.approval = None;
                    item.active_version = None;
                    item.previous_version = None;
                    item.benchmark_before = None;
                    item.benchmark_after = None;
                    item
                })
                .collect(),
            None,
        ),
        Err(error) => (Vec::new(), Some(error.to_string())),
    };
    let receipt = LegacyMigrationReceipt {
        schema_version: LEGACY_SCHEMA_VERSION,
        source_path: legacy.display().to_string(),
        source_digest: digest.clone(),
        imported_at,
        records: records.clone(),
        error,
    };
    fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let temporary_receipt = root.join(format!("migration-receipt.tmp-{}.json", std::process::id()));
    fs::write(
        &temporary_receipt,
        serde_json::to_vec_pretty(&receipt).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    fs::rename(&temporary_receipt, &receipt_path).map_err(|error| error.to_string())?;
    quarantine_legacy_if_present(&root, &legacy, &digest)?;
    Ok(records)
}

fn quarantine_legacy_if_present(root: &Path, legacy: &Path, digest: &str) -> Result<(), String> {
    if !legacy.is_file() {
        return Ok(());
    }
    let bytes = fs::read(legacy).map_err(|error| error.to_string())?;
    if repository_key_bytes(&bytes) != digest {
        return Err(
            "legacy improvement source changed after migration receipt was committed".into(),
        );
    }
    let quarantine = root.join("quarantine");
    fs::create_dir_all(&quarantine).map_err(|error| error.to_string())?;
    let destination = quarantine.join(format!("improvements-{digest}.json"));
    if destination.is_file() {
        fs::remove_file(legacy).map_err(|error| error.to_string())?;
    } else {
        fs::rename(legacy, destination).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn add_friction(map: &mut BTreeMap<String, (u32, Vec<String>)>, category: &str, id: &str) {
    let entry = map.entry(category.into()).or_default();
    entry.0 += 1;
    if entry.1.len() < 20 && !entry.1.iter().any(|value| value == id) {
        entry.1.push(id.into());
    }
}

fn rate(part: u32, total: u32) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 / total as f64 * 100.0
    }
}

fn average_duration_minutes(total_latency_millis: u64, total: u32) -> f64 {
    if total == 0 {
        0.0
    } else {
        total_latency_millis as f64 / total as f64 / 60_000.0
    }
}

fn unix_ms_to_timestamp(value: i64) -> String {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(value) * 1_000_000)
        .ok()
        .and_then(|time| time.format(&Rfc3339).ok())
        .unwrap_or_default()
}

fn unix_ms_to_date(value: i64) -> String {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(value) * 1_000_000)
        .map(|time| time.date().to_string())
        .unwrap_or_else(|_| "unknown".into())
}

fn repository_key_bytes(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn canonical_repo(repo: &str) -> Result<PathBuf, String> {
    let path = fs::canonicalize(Path::new(repo))
        .map_err(|error| format!("cannot open {repo}: {error}"))?;
    if !path.is_dir() {
        return Err("repository is not a directory".into());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_record() -> ImprovementRecord {
        ImprovementRecord {
            id: "legacy-1".into(),
            created_at: "2026-08-01T00:00:00Z".into(),
            updated_at: "2026-08-01T00:00:00Z".into(),
            title: "legacy candidate".into(),
            problem: "old dashboard record".into(),
            proposed_change: "old change".into(),
            evidence: vec!["legacy-evidence".into()],
            source_sessions: vec!["session-1".into()],
            risk: "unknown".into(),
            status: "adopted".into(),
            benchmark_before: Some(10.0),
            benchmark_after: Some(90.0),
            rollback_note: "old rollback".into(),
            revision: 3,
            approval: Some(ImprovementApproval {
                reviewer: "legacy-reviewer".into(),
                approved_at: "2026-08-01T00:00:00Z".into(),
                proposal_revision: 3,
            }),
            active_version: Some("v3".into()),
            previous_version: Some("v2".into()),
            conflicts_with: BTreeSet::new(),
            observations: Vec::new(),
            suspension_reason: None,
        }
    }

    #[test]
    fn arbitrary_session_text_does_not_create_friction() {
        let repo = crate::tempfile::tempdir().expect("repo");
        let sessions = repo.path().join(".medusa/sessions");
        fs::create_dir_all(&sessions).expect("sessions");
        fs::write(
            sessions.join("session.json"),
            br#"{"id":"session-1","completed":false,"message":"runtime failed verification failed retry"}"#,
        )
        .expect("session");
        let dashboard = runtime_engineering_dashboard(repo.path().display().to_string(), Some(90))
            .expect("dashboard");
        assert!(dashboard.friction.is_empty());
        assert_eq!(dashboard.total_tasks, 0);
    }

    #[test]
    fn average_duration_uses_authoritative_outcome_latency() {
        assert_eq!(average_duration_minutes(120_000, 2), 1.0);
        assert_eq!(average_duration_minutes(0, 0), 0.0);
    }

    #[test]
    fn legacy_records_are_quarantined_and_never_reported_as_active() {
        let repo = crate::tempfile::tempdir().expect("repo");
        let root = repo.path().join(".medusa/engineering");
        fs::create_dir_all(&root).expect("engineering");
        fs::write(
            root.join("improvements.json"),
            serde_json::to_vec(&vec![legacy_record()]).expect("legacy json"),
        )
        .expect("legacy record");
        let dashboard = runtime_engineering_dashboard(repo.path().display().to_string(), Some(90))
            .expect("dashboard");
        let record = dashboard.improvements.first().expect("legacy record");
        assert_eq!(record.status, "legacyUntrusted");
        assert!(record.approval.is_none());
        assert!(record.active_version.is_none());
        assert!(record.benchmark_before.is_none());
        assert!(!root.join("improvements.json").exists());
        assert!(root.join("migration-receipt.json").is_file());
        assert!(root.join("quarantine").is_dir());
    }

    #[test]
    fn interrupted_legacy_quarantine_is_recovered_from_receipt() {
        let repo = crate::tempfile::tempdir().expect("repo");
        let root = repo.path().join(".medusa/engineering");
        fs::create_dir_all(&root).expect("engineering");
        let legacy = root.join("improvements.json");
        let bytes = serde_json::to_vec(&vec![legacy_record()]).expect("legacy json");
        fs::write(&legacy, &bytes).expect("legacy record");
        let digest = repository_key_bytes(&bytes);
        let mut record = legacy_record();
        record.status = "legacyUntrusted".into();
        record.approval = None;
        record.active_version = None;
        record.previous_version = None;
        record.benchmark_before = None;
        record.benchmark_after = None;
        let receipt = LegacyMigrationReceipt {
            schema_version: LEGACY_SCHEMA_VERSION,
            source_path: legacy.display().to_string(),
            source_digest: digest.clone(),
            imported_at: "2026-08-13T00:00:00Z".into(),
            records: vec![record],
            error: None,
        };
        fs::write(
            root.join("migration-receipt.json"),
            serde_json::to_vec_pretty(&receipt).expect("receipt json"),
        )
        .expect("receipt");

        let records = read_legacy_compatibility(repo.path()).expect("recover migration");
        assert_eq!(records.len(), 1);
        assert!(!legacy.exists());
        assert!(
            root.join("quarantine")
                .join(format!("improvements-{digest}.json"))
                .is_file()
        );
    }
}
