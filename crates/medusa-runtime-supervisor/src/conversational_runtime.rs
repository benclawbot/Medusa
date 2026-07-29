//! Runtime-facing coordination for the persistent conversational supervisor.
//!
//! The coordinator keeps conversation controls separate from autonomous worker controls. Frontends
//! may stop speech or response generation without touching workers, while task cancellation is
//! always routed to the exact registered worker and terminates its process tree.

use std::collections::BTreeMap;

use crate::conversational::{
    CancellationDisposition, ConversationalSupervisor, RegistrySnapshot, SupervisorEvent,
    TaskRecord, TaskStatus,
};

pub trait WorkerHandle {
    type Error;

    fn pause(&mut self) -> Result<(), Self::Error>;
    fn resume(&mut self) -> Result<(), Self::Error>;
    fn cancel_response(&mut self) -> Result<(), Self::Error>;
    fn terminate_process_tree(&mut self) -> Result<(), Self::Error>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConversationControl {
    StopSpeech,
    CancelResponse,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoordinatorEvent {
    Supervisor(Box<SupervisorEvent>),
    SpeechStopped,
    ResponseCancelled,
    WorkerProcessTreeTerminated { task_id: String },
}

pub struct ConversationalRuntime<W: WorkerHandle> {
    supervisor: ConversationalSupervisor,
    workers: BTreeMap<String, W>,
}

impl<W: WorkerHandle> ConversationalRuntime<W> {
    pub fn new(conversation_id: impl Into<String>) -> Result<Self, &'static str> {
        Ok(Self {
            supervisor: ConversationalSupervisor::new(conversation_id)?,
            workers: BTreeMap::new(),
        })
    }

    pub fn restore(snapshot: RegistrySnapshot) -> Result<Self, &'static str> {
        Ok(Self {
            supervisor: ConversationalSupervisor::restore(snapshot)?,
            workers: BTreeMap::new(),
        })
    }

    pub fn supervisor(&self) -> &ConversationalSupervisor {
        &self.supervisor
    }

    pub fn snapshot(&self) -> RegistrySnapshot {
        self.supervisor.snapshot()
    }

    pub fn register_task(
        &mut self,
        task: TaskRecord,
        worker: W,
    ) -> Result<Vec<CoordinatorEvent>, &'static str> {
        if self.workers.contains_key(&task.id) {
            return Err("worker already exists for task");
        }
        let task_id = task.id.clone();
        let events = self.supervisor.create_task(task)?;
        self.workers.insert(task_id, worker);
        Ok(events
            .into_iter()
            .map(|event| CoordinatorEvent::Supervisor(Box::new(event)))
            .collect())
    }

    pub fn pause_task(
        &mut self,
        task_id: &str,
    ) -> Result<CoordinatorEvent, RuntimeControlError<W::Error>> {
        self.worker_mut(task_id)?
            .pause()
            .map_err(RuntimeControlError::Worker)?;
        let event = self
            .supervisor
            .transition(task_id, TaskStatus::Paused)
            .map_err(RuntimeControlError::Supervisor)?;
        Ok(CoordinatorEvent::Supervisor(Box::new(event)))
    }

    pub fn resume_task(
        &mut self,
        task_id: &str,
    ) -> Result<CoordinatorEvent, RuntimeControlError<W::Error>> {
        self.worker_mut(task_id)?
            .resume()
            .map_err(RuntimeControlError::Worker)?;
        let event = self
            .supervisor
            .transition(task_id, TaskStatus::Active)
            .map_err(RuntimeControlError::Supervisor)?;
        Ok(CoordinatorEvent::Supervisor(Box::new(event)))
    }

    pub fn conversation_control(
        &mut self,
        control: ConversationControl,
    ) -> Result<Vec<CoordinatorEvent>, RuntimeControlError<W::Error>> {
        match control {
            ConversationControl::StopSpeech => Ok(vec![CoordinatorEvent::SpeechStopped]),
            ConversationControl::CancelResponse => {
                for worker in self.workers.values_mut() {
                    worker
                        .cancel_response()
                        .map_err(RuntimeControlError::Worker)?;
                }
                Ok(vec![CoordinatorEvent::ResponseCancelled])
            }
        }
    }

    pub fn cancel_task(
        &mut self,
        task_id: &str,
        disposition: CancellationDisposition,
    ) -> Result<Vec<CoordinatorEvent>, RuntimeControlError<W::Error>> {
        self.worker_mut(task_id)?
            .terminate_process_tree()
            .map_err(RuntimeControlError::Worker)?;
        let update = self
            .supervisor
            .cancel_task(task_id, disposition)
            .map_err(RuntimeControlError::Supervisor)?;
        Ok(vec![
            CoordinatorEvent::WorkerProcessTreeTerminated {
                task_id: task_id.to_owned(),
            },
            CoordinatorEvent::Supervisor(Box::new(update)),
        ])
    }

    pub fn cancel_all(
        &mut self,
        disposition: CancellationDisposition,
    ) -> Result<Vec<CoordinatorEvent>, RuntimeControlError<W::Error>> {
        let task_ids: Vec<String> = self
            .supervisor
            .tasks()
            .filter(|task| !task.status.terminal())
            .map(|task| task.id.clone())
            .collect();
        let mut events = Vec::new();
        for task_id in task_ids {
            events.extend(self.cancel_task(&task_id, disposition.clone())?);
        }
        Ok(events)
    }

    fn worker_mut(&mut self, task_id: &str) -> Result<&mut W, RuntimeControlError<W::Error>> {
        self.workers
            .get_mut(task_id)
            .ok_or(RuntimeControlError::MissingWorker)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum RuntimeControlError<E> {
    MissingWorker,
    Supervisor(&'static str),
    Worker(E),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::conversational::{ResourceClaims, TaskStatus};

    #[derive(Default)]
    struct MockWorker {
        paused: bool,
        responses_cancelled: u32,
        process_tree_terminated: bool,
    }

    impl WorkerHandle for MockWorker {
        type Error = &'static str;

        fn pause(&mut self) -> Result<(), Self::Error> {
            self.paused = true;
            Ok(())
        }

        fn resume(&mut self) -> Result<(), Self::Error> {
            self.paused = false;
            Ok(())
        }

        fn cancel_response(&mut self) -> Result<(), Self::Error> {
            self.responses_cancelled += 1;
            Ok(())
        }

        fn terminate_process_tree(&mut self) -> Result<(), Self::Error> {
            self.process_tree_terminated = true;
            Ok(())
        }
    }

    fn task(id: &str) -> TaskRecord {
        TaskRecord {
            id: id.to_owned(),
            name: format!("Task {id}"),
            objective: format!("Complete task {id}"),
            constraints: Vec::new(),
            owner: None,
            dependencies: BTreeSet::new(),
            priority: 0,
            status: TaskStatus::Queued,
            phase: "planning".to_owned(),
            resources: ResourceClaims::default(),
            pending_approval: None,
            output_summary: None,
            cancellation_disposition: None,
            revision: 0,
        }
    }

    #[test]
    fn stopping_speech_does_not_change_worker_state() {
        let mut runtime = ConversationalRuntime::new("conversation").unwrap();
        runtime
            .register_task(task("a"), MockWorker::default())
            .unwrap();
        runtime
            .supervisor
            .transition("a", TaskStatus::Active)
            .unwrap();
        assert_eq!(
            runtime
                .conversation_control(ConversationControl::StopSpeech)
                .unwrap(),
            vec![CoordinatorEvent::SpeechStopped]
        );
        assert_eq!(
            runtime.supervisor().task("a").unwrap().status,
            TaskStatus::Active
        );
    }

    #[test]
    fn cancelling_one_task_does_not_cancel_another() {
        let mut runtime = ConversationalRuntime::new("conversation").unwrap();
        runtime
            .register_task(task("a"), MockWorker::default())
            .unwrap();
        runtime
            .register_task(task("b"), MockWorker::default())
            .unwrap();
        runtime
            .cancel_task("a", CancellationDisposition::ArchiveChanges)
            .unwrap();
        assert_eq!(
            runtime.supervisor().task("a").unwrap().status,
            TaskStatus::Cancelling
        );
        assert_eq!(
            runtime.supervisor().task("b").unwrap().status,
            TaskStatus::Queued
        );
        assert!(runtime.workers.get("a").unwrap().process_tree_terminated);
        assert!(!runtime.workers.get("b").unwrap().process_tree_terminated);
    }

    #[test]
    fn cancel_all_terminates_every_active_process_tree() {
        let mut runtime = ConversationalRuntime::new("conversation").unwrap();
        runtime
            .register_task(task("a"), MockWorker::default())
            .unwrap();
        runtime
            .register_task(task("b"), MockWorker::default())
            .unwrap();
        runtime
            .cancel_all(CancellationDisposition::RevertChanges)
            .unwrap();
        assert!(
            runtime
                .workers
                .values()
                .all(|worker| worker.process_tree_terminated)
        );
        assert!(
            runtime
                .supervisor()
                .tasks()
                .all(|task| task.status == TaskStatus::Cancelling)
        );
    }

    #[test]
    fn snapshot_restoration_keeps_tasks_but_requires_worker_reattachment() {
        let mut runtime = ConversationalRuntime::new("conversation").unwrap();
        runtime
            .register_task(task("a"), MockWorker::default())
            .unwrap();
        let mut restored =
            ConversationalRuntime::<MockWorker>::restore(runtime.snapshot()).unwrap();
        assert!(restored.supervisor().task("a").is_some());
        assert_eq!(
            restored.pause_task("a"),
            Err(RuntimeControlError::MissingWorker)
        );
    }
}
