//! Crash-safe façade for the learning-monitor projection.
//!
//! The underlying monitor remains a rebuildable projection over durable events. This façade adds
//! one missing recovery invariant: if the canonical refinement authority commits a lifecycle
//! change and the process dies before the monitor publishes its matching projection update, the
//! next open reconciles `active` state and predecessor lineage from that canonical authority while
//! holding the monitor's existing inter-process lock.

#[path = "learning_monitor.rs"]
mod original;

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::refinement_authority::RefinementAuthorityStore;

pub use original::{
    ArtifactMonitorState, AttributionMethod, AttributionReport, BeliefState, CohortKey,
    CohortReport, ExposureRecord, ExposureState, LearningMonitorError, LearningMonitorSnapshot,
    MonitorAction, MonitorActionKind, MonitorArtifactKind, MonitorResult, OutcomeRecord,
    OutcomeStatus,
};

#[derive(Debug)]
pub struct LearningMonitorStore {
    inner: original::LearningMonitorStore,
}

impl LearningMonitorStore {
    pub fn open(repo: &Path) -> Result<Self, LearningMonitorError> {
        let inner = original::LearningMonitorStore::open(repo)?;
        let snapshot = inner.snapshot();
        if reconcile_from_canonical_authority(repo, snapshot)? {
            drop(inner);
            return Ok(Self {
                inner: original::LearningMonitorStore::open(repo)?,
            });
        }
        Ok(Self { inner })
    }

    #[must_use]
    pub fn snapshot(&self) -> LearningMonitorSnapshot {
        self.inner.snapshot()
    }

    pub fn record_outcome(
        &mut self,
        repo: &Path,
        outcome: OutcomeRecord,
    ) -> Result<MonitorResult, LearningMonitorError> {
        self.inner.record_outcome(repo, outcome)
    }

    pub fn record_session_outcome(
        &mut self,
        repo: &Path,
        outcome: OutcomeRecord,
    ) -> Result<MonitorResult, LearningMonitorError> {
        self.inner.record_session_outcome(repo, outcome)
    }

    pub fn record_selection(
        repo: &Path,
        context: &crate::refinement_authority::SelectionContext,
        result: &crate::refinement_authority::SelectionResult,
        projection_revision: u64,
        now_unix_ms: i64,
    ) -> Result<usize, LearningMonitorError> {
        // Opening through the façade first repairs any authority/projection split-brain before a
        // new selection can be attributed to an artifact whose canonical lifecycle already moved.
        drop(Self::open(repo)?);
        original::LearningMonitorStore::record_selection(
            repo,
            context,
            result,
            projection_revision,
            now_unix_ms,
        )
    }
}

fn reconcile_from_canonical_authority(
    repo: &Path,
    mut snapshot: LearningMonitorSnapshot,
) -> Result<bool, LearningMonitorError> {
    if snapshot.artifacts.is_empty() {
        return Ok(false);
    }

    let authority = RefinementAuthorityStore::open(repo)
        .map_err(|error| LearningMonitorError::Authority(error.to_string()))?;
    let canonical = authority
        .snapshot()
        .map_err(|error| LearningMonitorError::Authority(error.to_string()))?;

    let mut changed = false;
    for artifact in &mut snapshot.artifacts {
        let Some(record) = canonical.records.iter().find(|record| {
            record.proposal_id == artifact.artifact_id && record.version == artifact.artifact_version
        }) else {
            continue;
        };

        let canonical_active = canonical.active.iter().any(|proposal| {
            proposal.id == artifact.artifact_id && proposal.version == artifact.artifact_version
        });
        if artifact.active != canonical_active {
            artifact.active = canonical_active;
            changed = true;
        }
        if artifact.predecessor_id != record.predecessor_proposal_id {
            artifact.predecessor_id = record.predecessor_proposal_id.clone();
            changed = true;
        }
        if artifact.predecessor_version != record.predecessor_version {
            artifact.predecessor_version = record.predecessor_version;
            changed = true;
        }
    }

    if !changed {
        return Ok(false);
    }

    snapshot.revision = snapshot.revision.saturating_add(1);
    persist_reconciled_snapshot(repo, &snapshot)?;
    Ok(true)
}

fn persist_reconciled_snapshot(
    repo: &Path,
    snapshot: &LearningMonitorSnapshot,
) -> Result<(), LearningMonitorError> {
    let root = repo.join(".medusa/learning-monitor");
    fs::create_dir_all(&root)?;
    let artifacts = snapshot
        .artifacts
        .iter()
        .cloned()
        .map(|artifact| {
            (
                format!("{}:{}", artifact.artifact_id, artifact.artifact_version),
                artifact,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let document = serde_json::json!({
        "schema_version": snapshot.schema_version,
        "revision": snapshot.revision,
        "artifacts": artifacts,
        "unattributed_outcomes": snapshot.unattributed_outcomes,
    });
    let event = serde_json::json!({
        "schema_version": snapshot.schema_version,
        "revision": snapshot.revision,
        "kind": "authority_reconciliation",
        "recorded_at_unix_ms": unix_ms(),
        "document": document,
    });

    let mut events = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("events.jsonl"))?;
    serde_json::to_writer(&mut events, &event)?;
    events.write_all(b"\n")?;
    events.sync_data()?;

    let state_path = root.join("state.json");
    let temporary = root.join(format!(
        "state.reconcile-{}-{}.tmp",
        std::process::id(),
        snapshot.revision
    ));
    let mut state = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    serde_json::to_writer_pretty(&mut state, &event["document"])?;
    state.write_all(b"\n")?;
    state.sync_all()?;
    drop(state);
    if state_path.exists() {
        fs::remove_file(&state_path)?;
    }
    fs::rename(temporary, state_path)?;
    Ok(())
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::refinement_authority::{ApprovalActorClass, RefinementAuthorityStore};
    use medusa_context::refinement::{
        EvaluationResult, EvidenceKind, EvidenceRef, ProposerMetadata, RefinementArtifactKind,
        RefinementContent, RefinementProposal, RefinementRisk, RefinementScope,
    };
    use tempfile::tempdir;

    fn proposal(id: &str, version: u64, value: &str) -> RefinementProposal {
        RefinementProposal {
            id: id.into(),
            version,
            artifact_kind: RefinementArtifactKind::RepositoryConvention,
            scope: RefinementScope::Repository,
            evidence: vec![EvidenceRef {
                id: format!("evidence-{id}"),
                kind: EvidenceKind::UserCorrection,
                trajectory_id: "trajectory".into(),
                start_sequence: 1,
                end_sequence: 1,
            }],
            before: None,
            after: RefinementContent::RepositoryConvention {
                key: "workflow".into(),
                value: value.into(),
            },
            rationale: "verified correction".into(),
            expected_outcome: "matching work improves".into(),
            proposer: ProposerMetadata {
                model: "test".into(),
                route: "test".into(),
                version: "1".into(),
            },
            risk: RefinementRisk::Low,
        }
    }

    fn activate(
        authority: &mut RefinementAuthorityStore,
        proposal: RefinementProposal,
        mut revision: u64,
    ) -> u64 {
        let id = proposal.id.clone();
        let version = proposal.version;
        let mut snapshot = authority.propose(proposal, revision).expect("proposal");
        snapshot = authority
            .validate(&id, version, snapshot.revision)
            .expect("validation");
        snapshot = authority
            .record_evaluation(
                &id,
                version,
                EvaluationResult {
                    evaluator: "test".into(),
                    validation_passed: true,
                    regression_passed: true,
                    effectiveness_passed: true,
                    notes: "passed".into(),
                },
                snapshot.revision,
            )
            .expect("evaluation");
        snapshot = authority
            .approve(
                &id,
                version,
                ApprovalActorClass::User,
                &format!("approval-{id}"),
                1,
                snapshot.revision,
            )
            .expect("approval");
        revision = authority
            .activate(&id, version, snapshot.revision)
            .expect("activation")
            .revision;
        revision
    }

    fn stale_artifact() -> ArtifactMonitorState {
        ArtifactMonitorState {
            artifact_id: "harmful".into(),
            artifact_version: 1,
            kind: MonitorArtifactKind::Prompt,
            active: true,
            predecessor_id: Some("previous".into()),
            predecessor_version: Some(1),
            belief: BeliefState::default(),
            exposures: Vec::new(),
            outcomes: Vec::new(),
            reports: Vec::new(),
            actions: Vec::new(),
        }
    }

    #[test]
    fn reopen_reconciles_rollback_committed_before_monitor_projection() {
        let repo = tempdir().expect("repo");
        let mut authority = RefinementAuthorityStore::open(repo.path()).expect("authority");
        let mut revision = activate(&mut authority, proposal("previous", 1, "safe"), 0);
        let mut snapshot = authority
            .propose(proposal("harmful", 1, "harmful"), revision)
            .expect("harmful proposal");
        snapshot = authority
            .validate("harmful", 1, snapshot.revision)
            .expect("harmful validation");
        snapshot = authority
            .record_evaluation(
                "harmful",
                1,
                EvaluationResult {
                    evaluator: "test".into(),
                    validation_passed: true,
                    regression_passed: true,
                    effectiveness_passed: true,
                    notes: "passed".into(),
                },
                snapshot.revision,
            )
            .expect("harmful evaluation");
        snapshot = authority
            .approve(
                "harmful",
                1,
                ApprovalActorClass::User,
                "approval-harmful",
                1,
                snapshot.revision,
            )
            .expect("harmful approval");
        snapshot = authority
            .supersede("previous", 1, "harmful", 1, snapshot.revision)
            .expect("supersede");
        revision = authority
            .activate("harmful", 1, snapshot.revision)
            .expect("harmful activation")
            .revision;
        authority
            .rollback(
                "harmful",
                1,
                Some("previous"),
                Some(1),
                "forced crash-window rollback",
                revision,
            )
            .expect("rollback");

        // Simulate the process dying after canonical rollback but before the monitor's matching
        // projection update: durable monitor evidence still says the harmful artifact is active.
        let stale = LearningMonitorSnapshot {
            schema_version: 1,
            revision: 7,
            artifacts: vec![stale_artifact()],
            unattributed_outcomes: Vec::new(),
        };
        persist_reconciled_snapshot(repo.path(), &stale).expect("stale monitor projection");

        let recovered = LearningMonitorStore::open(repo.path())
            .expect("reconciled open")
            .snapshot();
        assert_eq!(recovered.revision, 8);
        assert_eq!(recovered.artifacts.len(), 1);
        assert!(!recovered.artifacts[0].active);
        assert_eq!(recovered.artifacts[0].predecessor_id.as_deref(), Some("previous"));
        assert_eq!(recovered.artifacts[0].predecessor_version, Some(1));

        let events = fs::read_to_string(
            repo.path()
                .join(".medusa/learning-monitor/events.jsonl"),
        )
        .expect("events");
        assert!(events.contains("authority_reconciliation"));
    }
}
