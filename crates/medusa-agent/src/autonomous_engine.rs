impl<P: ModelProvider> AgentEngine<P> {
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

    pub fn start_role_aware_autonomous_execution(
        &self,
        session: &mut AgentSession,
        workers: Vec<(String, autonomous_execution::WorkerRole)>,
    ) -> MedusaResult<Vec<(String, String)>> {
        let workers = role_workers(workers)?;
        let mut execution = autonomous_execution::AutonomousExecution::start_with_roles(
            session,
            workers,
            3,
        )?;
        let assignments = execution.dispatch_ready(session)?;
        persist(session)?;
        Ok(assignment_pairs(assignments))
    }

    pub fn dispatch_autonomous_ready(
        &self,
        session: &mut AgentSession,
    ) -> MedusaResult<Vec<(String, String)>> {
        let mut execution = autonomous_execution::AutonomousExecution::load(session)?;
        let assignments = execution.dispatch_ready(session)?;
        persist(session)?;
        Ok(assignment_pairs(assignments))
    }

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

    pub fn submit_autonomous_task_for_review(
        &self,
        session: &mut AgentSession,
        task_id: &str,
        worker_id: &str,
        summary: String,
    ) -> MedusaResult<()> {
        let mut execution = autonomous_execution::AutonomousExecution::load(session)?;
        execution.submit_for_review(session, task_id, worker_id, summary)?;
        session.updated_at = OffsetDateTime::now_utc();
        persist(session)
    }

    pub fn review_autonomous_task(
        &self,
        session: &mut AgentSession,
        task_id: &str,
        reviewer_id: &str,
        approved: bool,
        feedback: String,
    ) -> MedusaResult<Vec<(String, String)>> {
        let mut execution = autonomous_execution::AutonomousExecution::load(session)?;
        execution.review(session, task_id, reviewer_id, approved, feedback)?;
        let assignments = execution.dispatch_ready(session)?;
        session.updated_at = OffsetDateTime::now_utc();
        session.completed = execution.is_complete();
        persist(session)?;
        Ok(assignment_pairs(assignments))
    }

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

fn autonomous_workers(worker_ids: Vec<String>) -> MedusaResult<Vec<dynamic_scheduler::Worker>> {
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
            validate_worker_id(&id)?;
            Ok(dynamic_scheduler::Worker {
                id,
                capabilities: vec!["coding".to_owned()],
                healthy: true,
                capacity: 1,
            })
        })
        .collect()
}

fn role_workers(
    workers: Vec<(String, autonomous_execution::WorkerRole)>,
) -> MedusaResult<Vec<autonomous_execution::AutonomousWorker>> {
    if workers.is_empty() {
        return Err(MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            "role-aware autonomous execution requires workers",
        ));
    }
    workers
        .into_iter()
        .map(|(id, role)| {
            validate_worker_id(&id)?;
            Ok(autonomous_execution::AutonomousWorker {
                id,
                role,
                capacity: 1,
            })
        })
        .collect()
}

fn validate_worker_id(id: &str) -> MedusaResult<()> {
    if id.trim().is_empty() {
        Err(MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            "autonomous worker identifiers cannot be empty",
        ))
    } else {
        Ok(())
    }
}

fn assignment_pairs(assignments: Vec<dynamic_scheduler::Assignment>) -> Vec<(String, String)> {
    assignments
        .into_iter()
        .map(|assignment| (assignment.task_id, assignment.worker_id))
        .collect()
}