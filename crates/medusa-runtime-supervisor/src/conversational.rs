//! Persistent conversational supervision for multiple autonomous tasks.
//!
//! This module deliberately contains no frontend, provider, or process-spawning code. It defines
//! the shared task registry and safety semantics consumed by voice, text, desktop, TUI, and the
//! execution runtime.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use tracing::{error, info, warn};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskStatus {
    Queued,
    Active,
    Blocked,
    ApprovalRequired,
    Paused,
    Cancelling,
    Cancelled,
    Completed,
    Failed,
}

impl TaskStatus {
    pub fn terminal(&self) -> bool {
        matches!(self, Self::Cancelled | Self::Completed | Self::Failed)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CancellationDisposition {
    RetainChanges,
    RevertChanges,
    ArchiveChanges,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResourceClaims {
    pub files: BTreeSet<String>,
    pub worktrees: BTreeSet<String>,
    pub branches: BTreeSet<String>,
    pub commands: BTreeSet<String>,
    pub processes: BTreeSet<u32>,
}

impl ResourceClaims {
    pub fn conflicting_files(&self, other: &Self) -> Vec<String> {
        self.files.intersection(&other.files).cloned().collect()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ApprovalBinding {
    pub approval_id: String,
    pub task_id: String,
    pub action_fingerprint: String,
    pub summary: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaskRecord {
    pub id: String,
    pub name: String,
    pub objective: String,
    pub constraints: Vec<String>,
    pub owner: Option<String>,
    pub dependencies: BTreeSet<String>,
    pub priority: i32,
    pub status: TaskStatus,
    pub phase: String,
    pub resources: ResourceClaims,
    pub pending_approval: Option<ApprovalBinding>,
    pub output_summary: Option<String>,
    pub cancellation_disposition: Option<CancellationDisposition>,
    pub revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum SupervisorEvent {
    TaskCreated {
        task: TaskRecord,
    },
    TaskUpdated {
        task: TaskRecord,
    },
    TaskRemoved {
        task_id: String,
    },
    ConflictDetected {
        first_task_id: String,
        second_task_id: String,
        files: Vec<String>,
    },
    ApprovalRequested {
        binding: ApprovalBinding,
    },
    ApprovalResolved {
        task_id: String,
        approval_id: String,
        approved: bool,
    },
    CancelAllRequested {
        task_ids: Vec<String>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RegistrySnapshot {
    pub conversation_id: String,
    pub tasks: BTreeMap<String, TaskRecord>,
    pub recent_task_ids: Vec<String>,
    pub sequence: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReferenceResolution {
    Resolved(String),
    Ambiguous(Vec<String>),
    Missing,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConversationalSupervisor {
    conversation_id: String,
    tasks: BTreeMap<String, TaskRecord>,
    recent_task_ids: Vec<String>,
    sequence: u64,
}

impl ConversationalSupervisor {
    pub fn new(conversation_id: impl Into<String>) -> Result<Self, &'static str> {
        let conversation_id = conversation_id.into();
        require_non_empty(&conversation_id, "conversation identifier cannot be empty")?;
        Ok(Self {
            conversation_id,
            tasks: BTreeMap::new(),
            recent_task_ids: Vec::new(),
            sequence: 0,
        })
    }

    pub fn restore(snapshot: RegistrySnapshot) -> Result<Self, &'static str> {
        require_non_empty(
            &snapshot.conversation_id,
            "conversation identifier cannot be empty",
        )?;
        let supervisor = Self {
            conversation_id: snapshot.conversation_id,
            tasks: snapshot.tasks,
            recent_task_ids: snapshot.recent_task_ids,
            sequence: snapshot.sequence,
        };
        supervisor.validate()?;
        Ok(supervisor)
    }

    pub fn snapshot(&self) -> RegistrySnapshot {
        RegistrySnapshot {
            conversation_id: self.conversation_id.clone(),
            tasks: self.tasks.clone(),
            recent_task_ids: self.recent_task_ids.clone(),
            sequence: self.sequence,
        }
    }

    pub fn tasks(&self) -> impl Iterator<Item = &TaskRecord> {
        self.tasks.values()
    }

    pub fn task(&self, task_id: &str) -> Option<&TaskRecord> {
        self.tasks.get(task_id)
    }

    pub fn create_task(&mut self, task: TaskRecord) -> Result<Vec<SupervisorEvent>, &'static str> {
        validate_task(&task)?;
        if self.tasks.contains_key(&task.id) {
            return Err("task identifier already exists");
        }
        for dependency in &task.dependencies {
            if !self.tasks.contains_key(dependency) {
                return Err("task dependency does not exist");
            }
        }

        let mut events = Vec::new();
        for existing in self
            .tasks
            .values()
            .filter(|candidate| !candidate.status.terminal())
        {
            let files = existing.resources.conflicting_files(&task.resources);
            if !files.is_empty() {
                warn!(
                    first_task_id = %existing.id,
                    second_task_id = %task.id,
                    conflicting_files = files.len(),
                    "supervisor task resource conflict detected"
                );
                events.push(SupervisorEvent::ConflictDetected {
                    first_task_id: existing.id.clone(),
                    second_task_id: task.id.clone(),
                    files,
                });
            }
        }
        self.touch(&task.id);
        let task_id = task.id.clone();
        self.tasks.insert(task.id.clone(), task.clone());
        self.bump()?;
        events.insert(0, SupervisorEvent::TaskCreated { task });
        info!(task_id = %task_id, events = events.len(), "supervisor task created");
        Ok(events)
    }

    pub fn transition(
        &mut self,
        task_id: &str,
        next: TaskStatus,
    ) -> Result<SupervisorEvent, &'static str> {
        let task = self.tasks.get_mut(task_id).ok_or("task does not exist")?;
        let previous = task.status.clone();
        let next_status = next.clone();
        if !valid_transition(&task.status, &next) {
            error!(task_id = %task_id, from = ?previous, to = ?next_status, "supervisor task transition rejected");
            return Err("invalid task status transition");
        }
        task.status = next;
        task.revision = task
            .revision
            .checked_add(1)
            .ok_or("task revision overflow")?;
        let task = task.clone();
        self.touch(task_id);
        self.bump()?;
        info!(task_id = %task_id, from = ?previous, to = ?next_status, revision = task.revision, "supervisor task transition applied");
        Ok(SupervisorEvent::TaskUpdated { task })
    }

    pub fn reprioritize(
        &mut self,
        task_id: &str,
        priority: i32,
    ) -> Result<SupervisorEvent, &'static str> {
        let task = self.tasks.get_mut(task_id).ok_or("task does not exist")?;
        if task.status.terminal() {
            return Err("terminal tasks cannot be reprioritized");
        }
        task.priority = priority;
        task.revision = task
            .revision
            .checked_add(1)
            .ok_or("task revision overflow")?;
        let task = task.clone();
        self.touch(task_id);
        self.bump()?;
        Ok(SupervisorEvent::TaskUpdated { task })
    }

    pub fn update_constraints(
        &mut self,
        task_id: &str,
        constraints: Vec<String>,
    ) -> Result<SupervisorEvent, &'static str> {
        if constraints.iter().any(|value| value.trim().is_empty()) {
            return Err("task constraints cannot be empty");
        }
        let task = self.tasks.get_mut(task_id).ok_or("task does not exist")?;
        if task.status.terminal() {
            return Err("terminal tasks cannot be redirected");
        }
        task.constraints = constraints;
        task.revision = task
            .revision
            .checked_add(1)
            .ok_or("task revision overflow")?;
        let task = task.clone();
        self.touch(task_id);
        self.bump()?;
        Ok(SupervisorEvent::TaskUpdated { task })
    }

    pub fn request_approval(
        &mut self,
        binding: ApprovalBinding,
    ) -> Result<Vec<SupervisorEvent>, &'static str> {
        validate_approval(&binding)?;
        let task = self
            .tasks
            .get_mut(&binding.task_id)
            .ok_or("approval task does not exist")?;
        if task.status.terminal() {
            return Err("terminal tasks cannot request approval");
        }
        if task.pending_approval.is_some() {
            return Err("task already has a pending approval");
        }
        task.pending_approval = Some(binding.clone());
        task.status = TaskStatus::ApprovalRequired;
        task.revision = task
            .revision
            .checked_add(1)
            .ok_or("task revision overflow")?;
        let updated = task.clone();
        self.touch(&binding.task_id);
        self.bump()?;
        Ok(vec![
            SupervisorEvent::ApprovalRequested { binding },
            SupervisorEvent::TaskUpdated { task: updated },
        ])
    }

    pub fn resolve_approval(
        &mut self,
        task_id: &str,
        approval_id: &str,
        approved: bool,
    ) -> Result<Vec<SupervisorEvent>, &'static str> {
        let task = self
            .tasks
            .get_mut(task_id)
            .ok_or("approval task does not exist")?;
        let binding = task
            .pending_approval
            .as_ref()
            .ok_or("task has no pending approval")?;
        if binding.task_id != task_id || binding.approval_id != approval_id {
            return Err("approval is not bound to this task and action");
        }
        task.pending_approval = None;
        task.status = if approved {
            TaskStatus::Active
        } else {
            TaskStatus::Blocked
        };
        task.revision = task
            .revision
            .checked_add(1)
            .ok_or("task revision overflow")?;
        let updated = task.clone();
        self.touch(task_id);
        self.bump()?;
        Ok(vec![
            SupervisorEvent::ApprovalResolved {
                task_id: task_id.to_owned(),
                approval_id: approval_id.to_owned(),
                approved,
            },
            SupervisorEvent::TaskUpdated { task: updated },
        ])
    }

    pub fn cancel_task(
        &mut self,
        task_id: &str,
        disposition: CancellationDisposition,
    ) -> Result<SupervisorEvent, &'static str> {
        let task = self.tasks.get_mut(task_id).ok_or("task does not exist")?;
        if task.status.terminal() {
            return Err("terminal task cannot be cancelled");
        }
        task.status = TaskStatus::Cancelling;
        task.cancellation_disposition = Some(disposition);
        task.revision = task
            .revision
            .checked_add(1)
            .ok_or("task revision overflow")?;
        let task = task.clone();
        self.touch(task_id);
        self.bump()?;
        Ok(SupervisorEvent::TaskUpdated { task })
    }

    pub fn cancel_all(
        &mut self,
        disposition: CancellationDisposition,
    ) -> Result<Vec<SupervisorEvent>, &'static str> {
        let ids: Vec<String> = self
            .tasks
            .values()
            .filter(|task| !task.status.terminal())
            .map(|task| task.id.clone())
            .collect();
        let mut events = vec![SupervisorEvent::CancelAllRequested {
            task_ids: ids.clone(),
        }];
        for id in ids {
            events.push(self.cancel_task(&id, disposition.clone())?);
        }
        Ok(events)
    }

    pub fn resolve_reference(&self, reference: &str) -> ReferenceResolution {
        let normalized = reference.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return ReferenceResolution::Missing;
        }
        if matches!(
            normalized.as_str(),
            "it" | "that" | "current" | "latest" | "recent"
        ) {
            return self
                .recent_task_ids
                .last()
                .cloned()
                .map(ReferenceResolution::Resolved)
                .unwrap_or(ReferenceResolution::Missing);
        }
        if self.tasks.contains_key(reference) {
            return ReferenceResolution::Resolved(reference.to_owned());
        }
        let matches: Vec<String> = self
            .tasks
            .values()
            .filter(|task| {
                task.name.to_ascii_lowercase() == normalized
                    || task.name.to_ascii_lowercase().contains(&normalized)
            })
            .map(|task| task.id.clone())
            .collect();
        match matches.len() {
            0 => ReferenceResolution::Missing,
            1 => ReferenceResolution::Resolved(matches[0].clone()),
            _ => ReferenceResolution::Ambiguous(matches),
        }
    }

    pub fn conflicts_for(&self, task_id: &str) -> Result<Vec<(String, Vec<String>)>, &'static str> {
        let task = self.tasks.get(task_id).ok_or("task does not exist")?;
        Ok(self
            .tasks
            .values()
            .filter(|candidate| candidate.id != task_id && !candidate.status.terminal())
            .filter_map(|candidate| {
                let files = task.resources.conflicting_files(&candidate.resources);
                (!files.is_empty()).then(|| (candidate.id.clone(), files))
            })
            .collect())
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        require_non_empty(
            &self.conversation_id,
            "conversation identifier cannot be empty",
        )?;
        for (id, task) in &self.tasks {
            if id != &task.id {
                return Err("task map key does not match task identifier");
            }
            validate_task(task)?;
            for dependency in &task.dependencies {
                if !self.tasks.contains_key(dependency) {
                    return Err("task dependency does not exist");
                }
            }
            if let Some(binding) = &task.pending_approval {
                validate_approval(binding)?;
                if binding.task_id != task.id || task.status != TaskStatus::ApprovalRequired {
                    return Err("pending approval is not bound to approval-required task");
                }
            }
        }
        if self
            .recent_task_ids
            .iter()
            .any(|id| !self.tasks.contains_key(id))
        {
            return Err("recent task reference does not exist");
        }
        Ok(())
    }

    fn touch(&mut self, task_id: &str) {
        self.recent_task_ids
            .retain(|candidate| candidate != task_id);
        self.recent_task_ids.push(task_id.to_owned());
        if self.recent_task_ids.len() > 32 {
            self.recent_task_ids.remove(0);
        }
    }

    fn bump(&mut self) -> Result<(), &'static str> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("supervisor sequence overflow")?;
        Ok(())
    }
}

fn valid_transition(current: &TaskStatus, next: &TaskStatus) -> bool {
    use TaskStatus::*;
    matches!(
        (current, next),
        (Queued, Active | Paused | Cancelling | Failed)
            | (
                Active,
                Blocked | ApprovalRequired | Paused | Cancelling | Completed | Failed
            )
            | (Blocked, Active | Paused | Cancelling | Failed)
            | (
                ApprovalRequired,
                Active | Blocked | Paused | Cancelling | Failed
            )
            | (Paused, Queued | Active | Cancelling | Failed)
            | (Cancelling, Cancelled | Failed)
    )
}

fn validate_task(task: &TaskRecord) -> Result<(), &'static str> {
    require_non_empty(&task.id, "task identifier cannot be empty")?;
    require_non_empty(&task.name, "task name cannot be empty")?;
    require_non_empty(&task.objective, "task objective cannot be empty")?;
    require_non_empty(&task.phase, "task phase cannot be empty")?;
    if task.dependencies.contains(&task.id) {
        return Err("task cannot depend on itself");
    }
    if task.constraints.iter().any(|value| value.trim().is_empty()) {
        return Err("task constraints cannot be empty");
    }
    Ok(())
}

fn validate_approval(binding: &ApprovalBinding) -> Result<(), &'static str> {
    require_non_empty(&binding.approval_id, "approval identifier cannot be empty")?;
    require_non_empty(&binding.task_id, "approval task identifier cannot be empty")?;
    require_non_empty(&binding.summary, "approval summary cannot be empty")?;
    if binding.action_fingerprint.len() != 64
        || !binding
            .action_fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("approval action fingerprint must be a SHA-256 hexadecimal digest");
    }
    Ok(())
}

fn require_non_empty(value: &str, message: &'static str) -> Result<(), &'static str> {
    if value.trim().is_empty() {
        Err(message)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, name: &str, files: &[&str]) -> TaskRecord {
        TaskRecord {
            id: id.to_owned(),
            name: name.to_owned(),
            objective: format!("Complete {name}"),
            constraints: Vec::new(),
            owner: None,
            dependencies: BTreeSet::new(),
            priority: 0,
            status: TaskStatus::Queued,
            phase: "planning".to_owned(),
            resources: ResourceClaims {
                files: files.iter().map(|value| (*value).to_owned()).collect(),
                ..ResourceClaims::default()
            },
            pending_approval: None,
            output_summary: None,
            cancellation_disposition: None,
            revision: 0,
        }
    }

    #[test]
    fn independent_tasks_transition_without_affecting_each_other() {
        let mut supervisor = ConversationalSupervisor::new("conversation").unwrap();
        supervisor
            .create_task(task("a", "Escape bug", &["src/input.rs"]))
            .unwrap();
        supervisor
            .create_task(task("b", "Session investigation", &["src/session.rs"]))
            .unwrap();
        supervisor.transition("a", TaskStatus::Active).unwrap();
        supervisor.transition("b", TaskStatus::Paused).unwrap();
        assert_eq!(supervisor.task("a").unwrap().status, TaskStatus::Active);
        assert_eq!(supervisor.task("b").unwrap().status, TaskStatus::Paused);
    }

    #[test]
    fn overlapping_files_emit_conflict_and_are_queryable() {
        let mut supervisor = ConversationalSupervisor::new("conversation").unwrap();
        supervisor
            .create_task(task("a", "First", &["src/lib.rs"]))
            .unwrap();
        let events = supervisor
            .create_task(task("b", "Second", &["src/lib.rs"]))
            .unwrap();
        assert!(events.iter().any(|event| matches!(event, SupervisorEvent::ConflictDetected { files, .. } if files == &["src/lib.rs"] )));
        assert_eq!(
            supervisor.conflicts_for("b").unwrap(),
            vec![("a".to_owned(), vec!["src/lib.rs".to_owned()])]
        );
    }

    #[test]
    fn approvals_cannot_leak_between_tasks() {
        let mut supervisor = ConversationalSupervisor::new("conversation").unwrap();
        supervisor.create_task(task("a", "First", &[])).unwrap();
        supervisor.create_task(task("b", "Second", &[])).unwrap();
        supervisor
            .request_approval(ApprovalBinding {
                approval_id: "approve-a".to_owned(),
                task_id: "a".to_owned(),
                action_fingerprint: "a".repeat(64),
                summary: "write file".to_owned(),
            })
            .unwrap();
        assert_eq!(
            supervisor.resolve_approval("b", "approve-a", true),
            Err("task has no pending approval")
        );
        assert_eq!(
            supervisor.resolve_approval("a", "wrong", true),
            Err("approval is not bound to this task and action")
        );
        supervisor.resolve_approval("a", "approve-a", true).unwrap();
        assert_eq!(supervisor.task("a").unwrap().status, TaskStatus::Active);
    }

    #[test]
    fn cancel_all_marks_only_non_terminal_tasks_and_records_disposition() {
        let mut supervisor = ConversationalSupervisor::new("conversation").unwrap();
        supervisor.create_task(task("a", "First", &[])).unwrap();
        supervisor.create_task(task("b", "Second", &[])).unwrap();
        supervisor.transition("a", TaskStatus::Active).unwrap();
        supervisor.transition("a", TaskStatus::Completed).unwrap();
        supervisor
            .cancel_all(CancellationDisposition::ArchiveChanges)
            .unwrap();
        assert_eq!(supervisor.task("a").unwrap().status, TaskStatus::Completed);
        assert_eq!(supervisor.task("b").unwrap().status, TaskStatus::Cancelling);
        assert_eq!(
            supervisor.task("b").unwrap().cancellation_disposition,
            Some(CancellationDisposition::ArchiveChanges)
        );
    }

    #[test]
    fn references_resolve_by_id_name_recent_and_report_ambiguity() {
        let mut supervisor = ConversationalSupervisor::new("conversation").unwrap();
        supervisor
            .create_task(task("a", "Session persistence", &[]))
            .unwrap();
        supervisor
            .create_task(task("b", "Session cleanup", &[]))
            .unwrap();
        assert_eq!(
            supervisor.resolve_reference("a"),
            ReferenceResolution::Resolved("a".to_owned())
        );
        assert_eq!(
            supervisor.resolve_reference("cleanup"),
            ReferenceResolution::Resolved("b".to_owned())
        );
        assert_eq!(
            supervisor.resolve_reference("it"),
            ReferenceResolution::Resolved("b".to_owned())
        );
        assert_eq!(
            supervisor.resolve_reference("session"),
            ReferenceResolution::Ambiguous(vec!["a".to_owned(), "b".to_owned()])
        );
    }

    #[test]
    fn snapshot_restores_frontend_disconnect_state() {
        let mut supervisor = ConversationalSupervisor::new("conversation").unwrap();
        supervisor
            .create_task(task("a", "Persistent task", &[]))
            .unwrap();
        supervisor.transition("a", TaskStatus::Active).unwrap();
        let restored = ConversationalSupervisor::restore(supervisor.snapshot()).unwrap();
        assert_eq!(restored.task("a").unwrap().status, TaskStatus::Active);
        assert_eq!(
            restored.resolve_reference("recent"),
            ReferenceResolution::Resolved("a".to_owned())
        );
    }
}
