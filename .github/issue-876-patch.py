from pathlib import Path

root_path = Path('crates/medusa-session-continuity/src/root.rs')
root = root_path.read_text()

root = root.replace(
    '''pub struct RepairAttemptCheckpoint {
    pub id: String,
    pub failure_fingerprint: String,
    pub changed_files: Vec<String>,
    pub outcome: VerificationOutcome,
}''',
    '''pub struct RepairAttemptCheckpoint {
    pub id: String,
    pub failure_fingerprint: String,
    pub changed_files: Vec<String>,
    pub outcome: VerificationOutcome,
    #[serde(default)]
    pub hypothesis: String,
    #[serde(default)]
    pub repository_fingerprint: String,
}''',
)

marker = '''pub struct FailureCheckpoint {
    pub fingerprint: String,
    pub classification: String,
    pub summary: String,
    pub repairs: Vec<RepairAttemptCheckpoint>,
}
'''
addition = marker + '''
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairLedgerTransition {
    New,
    Persisted,
    Resolved,
    Transformed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairLedgerEntry {
    pub fingerprint: String,
    pub source: String,
    pub command: String,
    pub scope: String,
    pub file: Option<String>,
    pub symbol: Option<String>,
    pub test: Option<String>,
    pub diagnostic_class: String,
    pub summary: String,
    pub first_generation: u64,
    pub last_generation: u64,
    pub occurrence_count: u32,
    #[serde(default)]
    pub changed_details: Vec<String>,
    #[serde(default)]
    pub source_refs: Vec<String>,
    pub root_fingerprint: Option<String>,
    pub cascade: bool,
    pub transition: RepairLedgerTransition,
    #[serde(default)]
    pub repairs: Vec<RepairAttemptCheckpoint>,
}

impl RepairLedgerEntry {
    pub fn unresolved(&self) -> bool {
        matches!(
            self.transition,
            RepairLedgerTransition::New
                | RepairLedgerTransition::Persisted
                | RepairLedgerTransition::Transformed
        )
    }
}
'''
if marker not in root:
    raise SystemExit('FailureCheckpoint marker missing')
root = root.replace(marker, addition, 1)

old = '''    pub failure_history: Vec<FailureCheckpoint>,
    pub disproved_hypotheses: Vec<DisprovedHypothesisCheckpoint>,'''
new = '''    pub failure_history: Vec<FailureCheckpoint>,
    #[serde(default)]
    pub repair_ledger: Vec<RepairLedgerEntry>,
    #[serde(default)]
    pub verification_generation: u64,
    #[serde(default)]
    pub repair_ledger_cursor: u64,
    pub disproved_hypotheses: Vec<DisprovedHypothesisCheckpoint>,'''
if old not in root:
    raise SystemExit('trajectory fields marker missing')
root = root.replace(old, new, 1)

old = '''            failure_history: Vec::new(),
            disproved_hypotheses: Vec::new(),'''
new = '''            failure_history: Vec::new(),
            repair_ledger: Vec::new(),
            verification_generation: 0,
            repair_ledger_cursor: 0,
            disproved_hypotheses: Vec::new(),'''
if old not in root:
    raise SystemExit('default marker missing')
root = root.replace(old, new, 1)

old = '''            self.failure_history.len(),
            self.disproved_hypotheses.len(),'''
new = '''            self.failure_history.len(),
            self.repair_ledger.len(),
            self.disproved_hypotheses.len(),'''
if old not in root:
    raise SystemExit('validation marker missing')
root = root.replace(old, new, 1)

method_marker = '''    pub fn allows_hypothesis_attempt(&self, signature: &str, repository_fingerprint: &str) -> bool {
        !self.disproved_hypotheses.iter().any(|item| {
            item.signature == signature && item.repository_fingerprint == repository_fingerprint
        })
    }
'''
method_addition = method_marker + '''
    pub fn allows_repair_attempt(
        &self,
        failure_fingerprint: &str,
        changed_files: &[String],
        hypothesis: &str,
        repository_fingerprint: &str,
    ) -> bool {
        !self.repair_ledger.iter().any(|failure| {
            failure.fingerprint == failure_fingerprint
                && failure.unresolved()
                && failure.repairs.iter().any(|repair| {
                    repair.changed_files == changed_files
                        && repair.hypothesis == hypothesis
                        && repair.repository_fingerprint == repository_fingerprint
                        && repair.outcome == VerificationOutcome::Failed
                })
        })
    }
'''
if method_marker not in root:
    raise SystemExit('retry guard marker missing')
root = root.replace(method_marker, method_addition, 1)
root_path.write_text(root)

trajectory_path = Path('crates/medusa-runtime/src/coding_trajectory.rs')
trajectory = trajectory_path.read_text()
old = '''use crate::RuntimeError;

const CONTEXT_LIMIT: usize = 12_000;'''
new = '''use crate::RuntimeError;

#[path = "repair_ledger.rs"]
mod repair_ledger;

const CONTEXT_LIMIT: usize = 12_000;'''
if old not in trajectory:
    raise SystemExit('module marker missing')
trajectory = trajectory.replace(old, new, 1)

marker = '''    trajectory.continuation_intent = session
        .plan
        .iter()
        .find(|step| step.status != AgentPlanStepStatus::Completed)
        .map(|step| format!("continue plan step: {}", step.title));
    Ok(trajectory)
}'''
replacement = '''    let repair_projection = repair_ledger::project(
        session,
        repository.workspace_fingerprint.as_str(),
        trajectory.repair_ledger.as_slice(),
        trajectory.verification_generation,
        trajectory.repair_ledger_cursor,
    );
    trajectory.repair_ledger = repair_projection.entries;
    trajectory.verification_generation = repair_projection.generation;
    trajectory.repair_ledger_cursor = repair_projection.cursor;
    trajectory.continuation_intent = session
        .plan
        .iter()
        .find(|step| step.status != AgentPlanStepStatus::Completed)
        .map(|step| format!("continue plan step: {}", step.title));
    Ok(trajectory)
}'''
if marker not in trajectory:
    raise SystemExit('projection marker missing')
trajectory = trajectory.replace(marker, replacement, 1)

old = '''        "[medusa-coding-trajectory-v1]\\nAuthoritative compact trajectory derived from the canonical journal. Preserve immutable objective/constraints, continue from retained plan/failure/verification state, do not retry disproved hypotheses on the same repository fingerprint without new evidence, and revalidate stale paths after repository drift.\\n{}",'''
new = '''        "[medusa-coding-trajectory-v1]\\nAuthoritative compact trajectory derived from the canonical journal. Preserve immutable objective/constraints and use repair_ledger as the complete actionable failure set. Repair all independent diagnostics from the latest verification generation together, expand exact source_refs only when needed, rerun the narrowest authoritative check after mutation, and do not repeat an identical failed repair on an unchanged repository fingerprint; re-plan or escalate instead. Revalidate stale paths after repository drift.\\n{}",'''
if old not in trajectory:
    raise SystemExit('render marker missing')
trajectory = trajectory.replace(old, new, 1)

marker = '''    #[test]
    fn journal_projection_survives_restart_and_repository_drift() {'''
new_test = '''    #[test]
    fn repair_ledger_collects_full_generation_deduplicates_and_resolves() {
        let repo = tempfile::tempdir().expect("repo");
        Command::new("git").arg("init").arg(repo.path()).status().expect("git init");
        let engine = AgentEngine::new(UnusedProvider, Config::default());
        let mut session = engine
            .create_session(repo.path(), "repair simultaneous diagnostics".to_owned())
            .expect("session");
        let diagnostics = vec![r#"$ cargo check
error[E0308]: mismatched types
  --> crates/a/src/lib.rs:12:3
error[E0425]: cannot find value `x`
  --> crates/b/src/lib.rs:8:9"#.to_owned()];
        medusa_agent::record_session_event(
            &mut session,
            Actor::System("verifier".to_owned()),
            EventPayload::VerificationCompleted {
                passed: false,
                evidence: diagnostics.clone(),
            },
        )
        .expect("first verification");
        sync_and_render(repo.path(), &session, None).expect("first sync");
        let first = store(repo.path(), session.id.as_str()).load().expect("stored");
        let first_trajectory = first.task.coding_trajectory.as_ref().expect("trajectory");
        assert_eq!(first_trajectory.verification_generation, 1);
        assert_eq!(first_trajectory.repair_ledger.len(), 2);
        assert!(first_trajectory.repair_ledger.iter().all(|entry| entry.occurrence_count == 1));
        assert!(first_trajectory.repair_ledger.iter().all(|entry| !entry.source_refs.is_empty()));

        medusa_agent::record_session_event(
            &mut session,
            Actor::System("verifier".to_owned()),
            EventPayload::VerificationCompleted {
                passed: false,
                evidence: diagnostics,
            },
        )
        .expect("repeat verification");
        sync_and_render(repo.path(), &session, None).expect("repeat sync");
        let repeated = store(repo.path(), session.id.as_str()).load().expect("stored repeat");
        let repeated_trajectory = repeated.task.coding_trajectory.as_ref().expect("trajectory");
        assert_eq!(repeated_trajectory.verification_generation, 2);
        assert_eq!(repeated_trajectory.repair_ledger.len(), 2);
        assert!(repeated_trajectory.repair_ledger.iter().all(|entry| entry.occurrence_count == 2));

        medusa_agent::record_session_event(
            &mut session,
            Actor::System("verifier".to_owned()),
            EventPayload::VerificationCompleted {
                passed: true,
                evidence: vec!["$ cargo check".to_owned()],
            },
        )
        .expect("passing verification");
        sync_and_render(repo.path(), &session, None).expect("passing sync");
        let passed = store(repo.path(), session.id.as_str()).load().expect("stored pass");
        let passed_trajectory = passed.task.coding_trajectory.as_ref().expect("trajectory");
        assert_eq!(passed_trajectory.verification_generation, 3);
        assert!(passed_trajectory.repair_ledger.iter().all(|entry| !entry.unresolved()));

        medusa_agent::compact_session(&mut session, Some("repair simultaneous diagnostics"))
            .expect("compaction");
        let restored = restore_for_resume(repo.path(), &session, false)
            .expect("restore")
            .expect("context");
        assert!(restored.contains("repair_ledger"));
        let restored_state = store(repo.path(), session.id.as_str()).load().expect("restored state");
        assert_eq!(
            restored_state
                .task
                .coding_trajectory
                .as_ref()
                .expect("trajectory")
                .verification_generation,
            3
        );
    }

    #[test]
    fn journal_projection_survives_restart_and_repository_drift() {'''
if marker not in trajectory:
    raise SystemExit('test marker missing')
trajectory = trajectory.replace(marker, new_test, 1)
trajectory_path.write_text(trajectory)
