impl<P: ModelProvider> AgentEngine<P> {
    /// Start a durable autonomous run from the current visible plan and dispatch ready work.
    pub fn start_autonomous_execution(
        &self,
        session: &mut AgentSession,
        worker_ids: Vec<String>,
    ) -> MedusaResult<Vec<(String, String)>> {
        let workers = autonomous_workers(worker_ids)?;
        let mut execution = autonomous_execution::AutonomousExecution::start(session, workers)?;
        let assignments = execution.dispatch_ready(session)?;
        persist(session)?;
        Ok(assignment_pairs(assignments))
    }

    /// Resume a persisted autonomous run and dispatch newly unblocked work.
    pub fn dispatch_autonomous_ready(
        &self,
        session: &mut AgentSession,
    ) -> MedusaResult<Vec<(String, String)>> {
        let mut execution = autonomous_execution::AutonomousExecution::load(session)?;
        let assignments = execution.dispatch_ready(session)?;
        persist(session)?;
        Ok(assignment_pairs(assignments))
    }

    /// Record successful worker completion and release dependent plan steps.
    pub fn complete_autonomous_task(
        &self,
        session: &mut AgentSession,
        task_id: &str,
        worker_id: &str,
    ) -> MedusaResult<()> {
        let mut execution = autonomous_execution::AutonomousExecution::load(session)?;
        execution.complete(session, task_id, worker_id)?;
        session.updated_at = OffsetDateTime::now_utc();
        session.completed = execution.is_complete();
        persist(session)
    }

    /// Record a worker failure. Retryable work is requeued until the durable limit is reached.
    pub fn fail_autonomous_task(
        &self,
        session: &mut AgentSession,
        task_id: &str,
        worker_id: &str,
        reason: String,
        retryable: bool,
    ) -> MedusaResult<Vec<String>> {
        let mut execution = autonomous_execution::AutonomousExecution::load(session)?;
        execution.fail(session, task_id, worker_id, reason, retryable)?;
        session.updated_at = OffsetDateTime::now_utc();
        let blocked = execution.blocked_tasks();
        persist(session)?;
        Ok(blocked)
    }

    /// Quarantine or restore a worker and requeue any interrupted work.
    pub fn set_autonomous_worker_health(
        &self,
        session: &mut AgentSession,
        worker_id: &str,
        healthy: bool,
    ) -> MedusaResult<()> {
        let mut execution = autonomous_execution::AutonomousExecution::load(session)?;
        execution.set_worker_health(session, worker_id, healthy)?;
        session.updated_at = OffsetDateTime::now_utc();
        persist(session)
    }
}

fn autonomous_workers(worker_ids: Vec<String>) -> MedusaResult<Vec<medusa_multi_agent_scheduler::Worker>> {
    if worker_ids.is_empty() {
        return Err(MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            "autonomous execution requires at least one worker",
        ));
    }
    worker_ids
        .into_iter()
        .map(|id| {
            if id.trim().is_empty() {
                return Err(MedusaError::new(
                    ErrorCode::InvalidConfiguration,
                    ErrorCategory::Validation,
                    "autonomous worker identifiers cannot be empty",
                ));
            }
            Ok(medusa_multi_agent_scheduler::Worker {
                id,
                capabilities: vec!["coding".to_owned()],
                healthy: true,
                capacity: 1,
            })
        })
        .collect()
}

fn assignment_pairs(
    assignments: Vec<medusa_multi_agent_scheduler::Assignment>,
) -> Vec<(String, String)> {
    assignments
        .into_iter()
        .map(|assignment| (assignment.task_id, assignment.worker_id))
        .collect()
}
