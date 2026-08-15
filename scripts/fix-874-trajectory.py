from pathlib import Path

p = Path("crates/medusa-session-continuity/src/root.rs")
s = p.read_text()
s = s.replace("pub const CURRENT_SCHEMA_VERSION: u32 = 2;", """pub const CURRENT_SCHEMA_VERSION: u32 = 3;
pub const CODING_TRAJECTORY_SCHEMA_VERSION: u32 = 1;
const MAX_TRAJECTORY_ITEMS: usize = 256;
const MAX_TRAJECTORY_TEXT_BYTES: usize = 16 * 1024;""")
marker = "#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]\npub struct AuthoritativeTaskState {"
insert = r'''#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEdge {
    pub parent: String,
    pub child: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepStatus { Pending, Active, Completed, Blocked }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStepCheckpoint {
    pub id: String,
    pub description: String,
    pub status: PlanStepStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationStatus { Running, PendingJoin, Joined, Failed }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegatedWorkCheckpoint {
    pub id: String,
    pub parent_task: String,
    pub summary: String,
    pub status: DelegationStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationOutcome { Passed, Failed, Interrupted }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationReceipt {
    pub command: String,
    pub outcome: VerificationOutcome,
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodingTrajectoryCheckpoint {
    pub schema_version: u32,
    pub task_stack: Vec<String>,
    pub task_graph: Vec<TaskEdge>,
    pub plan_steps: Vec<PlanStepCheckpoint>,
    pub delegations: Vec<DelegatedWorkCheckpoint>,
    pub modified_files: Vec<String>,
    pub verification_receipts: Vec<VerificationReceipt>,
    pub unresolved_uncertainties: Vec<String>,
    pub continuation_intent: Option<String>,
    pub resume_hops: u32,
}

impl Default for CodingTrajectoryCheckpoint {
    fn default() -> Self {
        Self {
            schema_version: CODING_TRAJECTORY_SCHEMA_VERSION,
            task_stack: Vec::new(), task_graph: Vec::new(), plan_steps: Vec::new(),
            delegations: Vec::new(), modified_files: Vec::new(), verification_receipts: Vec::new(),
            unresolved_uncertainties: Vec::new(), continuation_intent: None, resume_hops: 0,
        }
    }
}

impl CodingTrajectoryCheckpoint {
    pub fn validate(&self) -> Result<(), ContinuityError> {
        if self.schema_version != CODING_TRAJECTORY_SCHEMA_VERSION {
            return Err(ContinuityError::UnsupportedTrajectorySchema { found: self.schema_version, current: CODING_TRAJECTORY_SCHEMA_VERSION });
        }
        let lengths = [self.task_stack.len(), self.task_graph.len(), self.plan_steps.len(), self.delegations.len(), self.modified_files.len(), self.verification_receipts.len(), self.unresolved_uncertainties.len()];
        if lengths.into_iter().any(|len| len > MAX_TRAJECTORY_ITEMS) || serde_json::to_vec(self)?.len() > MAX_TRAJECTORY_TEXT_BYTES {
            return Err(ContinuityError::TrajectoryTooLarge);
        }
        Ok(())
    }

    pub fn restored_for_resume(&self) -> Result<Self, ContinuityError> {
        self.validate()?;
        let mut restored = self.clone();
        restored.resume_hops = restored.resume_hops.saturating_add(1);
        Ok(restored)
    }
}

'''
if marker not in s: raise SystemExit("task marker missing")
s = s.replace(marker, insert + marker, 1)
s = s.replace("    pub completion_status: Option<String>,\n}", "    pub completion_status: Option<String>,\n    #[serde(default)]\n    pub coding_trajectory: Option<CodingTrajectoryCheckpoint>,\n}", 1)
s = s.replace("    CompletionRecorded {\n        status: String,\n    },", "    CompletionRecorded {\n        status: String,\n    },\n    RepairLoopCheckpointed,\n    CompactionCheckpointed,\n    TrajectoryRestored { resume_hops: u32 },", 1)
s = s.replace('    #[error("session attachment state is inconsistent")]\n    InvalidAttachmentState,', '    #[error("session attachment state is inconsistent")]\n    InvalidAttachmentState,\n    #[error("unsupported coding trajectory schema version {found}; current version is {current}")]\n    UnsupportedTrajectorySchema { found: u32, current: u32 },\n    #[error("coding trajectory exceeds bounded checkpoint limits")]\n    TrajectoryTooLarge,', 1)
s = s.replace('                "completion_status": null,\n            })', '                "completion_status": null,\n                "coding_trajectory": null,\n            })', 1)
s = s.replace('    object.insert(\n        "schema_version".to_owned(),', '    if let Some(task) = object.get_mut("task").and_then(serde_json::Value::as_object_mut) {\n        task.entry("coding_trajectory").or_insert(serde_json::Value::Null);\n    }\n    object.insert(\n        "schema_version".to_owned(),', 1)
s = s.replace('    for (expected, event) in session.events.iter().enumerate() {', '    if let Some(trajectory) = &session.task.coding_trajectory { trajectory.validate()?; }\n    for (expected, event) in session.events.iter().enumerate() {', 1)
s = s.replace('            completion_status: status.map(str::to_owned),\n        }', '            completion_status: status.map(str::to_owned),\n            coding_trajectory: None,\n        }', 1)
s += r'''

#[cfg(test)]
mod coding_trajectory_tests {
    use super::*;

    fn trajectory() -> CodingTrajectoryCheckpoint {
        CodingTrajectoryCheckpoint {
            task_stack: vec!["issue-874".into(), "resume".into()],
            task_graph: vec![TaskEdge { parent: "issue-874".into(), child: "resume".into() }],
            plan_steps: vec![PlanStepCheckpoint { id: "schema".into(), description: "persist trajectory".into(), status: PlanStepStatus::Completed }, PlanStepCheckpoint { id: "resume".into(), description: "restore after compaction".into(), status: PlanStepStatus::Active }],
            delegations: vec![DelegatedWorkCheckpoint { id: "worker-1".into(), parent_task: "resume".into(), summary: "inspect recovery".into(), status: DelegationStatus::PendingJoin }],
            modified_files: vec!["crates/medusa-session-continuity/src/root.rs".into()],
            verification_receipts: vec![VerificationReceipt { command: "cargo test -p medusa-session-continuity".into(), outcome: VerificationOutcome::Failed, evidence: Some("resume mismatch".into()) }],
            unresolved_uncertainties: vec!["pending worker join".into()],
            continuation_intent: Some("join worker, repair verifier, rerun".into()),
            ..Default::default()
        }
    }

    fn task(value: CodingTrajectoryCheckpoint) -> AuthoritativeTaskState {
        AuthoritativeTaskState { plan_state: Some("running".into()), active_step: Some("resume".into()), coding_trajectory: Some(value), ..Default::default() }
    }

    #[test]
    fn trajectory_survives_repair_compaction_and_multi_hop_resume() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ContinuityStore::new(temp.path().join("session.json"));
        let initial = store.create("trajectory").expect("create");
        let attached = store.attach(AttachRequest { client_id: "daemon".into(), client_kind: ClientKind::Daemon, requested_mode: AttachmentMode::Owner, expected_revision: initial.revision, journal_cursor: 0, occurred_at_unix_ms: 1, event_id: "attach".into() }).expect("attach").session().clone();
        let original = trajectory();
        let repair = store.mutate(MutationRequest { client_id: "daemon".into(), expected_revision: attached.revision, occurred_at_unix_ms: 2, event_id: "repair".into(), event: SessionEventKind::RepairLoopCheckpointed, task: task(original.clone()) }).expect("repair").session().clone();
        let compacted = store.mutate(MutationRequest { client_id: "daemon".into(), expected_revision: repair.revision, occurred_at_unix_ms: 3, event_id: "compact".into(), event: SessionEventKind::CompactionCheckpointed, task: repair.task.clone() }).expect("compact").session().clone();
        assert_eq!(store.load().expect("load").task, compacted.task);
        let first = compacted.task.coding_trajectory.as_ref().expect("trajectory").restored_for_resume().expect("resume1");
        let resumed = store.mutate(MutationRequest { client_id: "daemon".into(), expected_revision: compacted.revision, occurred_at_unix_ms: 4, event_id: "resume".into(), event: SessionEventKind::TrajectoryRestored { resume_hops: first.resume_hops }, task: task(first) }).expect("resume mutation").session().clone();
        let second = resumed.task.coding_trajectory.as_ref().expect("trajectory").restored_for_resume().expect("resume2");
        assert_eq!(second.resume_hops, 2);
        assert_eq!(second.delegations, original.delegations);
        assert_eq!(second.verification_receipts, original.verification_receipts);
        assert_eq!(second.unresolved_uncertainties, original.unresolved_uncertainties);
        assert_eq!(second.continuation_intent, original.continuation_intent);
        assert_eq!(second.modified_files, original.modified_files);
        assert_eq!(second.plan_steps, original.plan_steps);
    }

    #[test]
    fn rejects_incompatible_and_unbounded_trajectory_checkpoints() {
        let mut incompatible = trajectory(); incompatible.schema_version += 1;
        assert!(matches!(incompatible.validate(), Err(ContinuityError::UnsupportedTrajectorySchema { .. })));
        let mut oversized = trajectory(); oversized.task_stack = (0..=MAX_TRAJECTORY_ITEMS).map(|i| format!("task-{i}")).collect();
        assert!(matches!(oversized.validate(), Err(ContinuityError::TrajectoryTooLarge)));
    }
}
'''
p.write_text(s)
