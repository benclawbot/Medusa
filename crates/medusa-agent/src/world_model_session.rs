use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_world_model::{
    Confidence, Experiment, HypothesisStatus, ObservationSource, WorkspaceModel, load, persist,
};

use crate::session::AgentSession;

/// Loads the session world model or returns a validation error when the session has none.
pub fn load_model(session: &AgentSession) -> MedusaResult<WorkspaceModel> {
    let reference = session.world_model.as_ref().ok_or_else(|| {
        MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            "session does not have a world model",
        )
    })?;
    load(&session.repo, reference).map_err(world_model_error)
}

/// Records a user-provided fact and returns its durable observation ID.
pub fn record_user_observation(
    session: &mut AgentSession,
    statement: impl Into<String>,
) -> MedusaResult<String> {
    mutate(session, |model| {
        Ok(model.record_observation(ObservationSource::UserStatement, statement))
    })
}

/// Adds an evidence-linked hypothesis and returns its durable hypothesis ID.
pub fn add_hypothesis(
    session: &mut AgentSession,
    statement: impl Into<String>,
    supporting_observations: Vec<String>,
) -> MedusaResult<String> {
    mutate(session, |model| {
        model
            .add_hypothesis(statement, supporting_observations)
            .map_err(world_model_error)
    })
}

/// Changes a hypothesis state after the caller has evaluated its evidence.
pub fn transition_hypothesis(
    session: &mut AgentSession,
    hypothesis_id: &str,
    status: HypothesisStatus,
    confidence: Confidence,
) -> MedusaResult<()> {
    mutate(session, |model| {
        model
            .transition_hypothesis(hypothesis_id, status, confidence)
            .map_err(world_model_error)
    })
}

/// Adds a prediction-bearing experiment to the session model.
pub fn add_experiment(session: &mut AgentSession, experiment: Experiment) -> MedusaResult<()> {
    mutate(session, |model| {
        model.add_experiment(experiment).map_err(world_model_error)
    })
}

fn mutate<T>(
    session: &mut AgentSession,
    operation: impl FnOnce(&mut WorkspaceModel) -> MedusaResult<T>,
) -> MedusaResult<T> {
    let reference = session.world_model.as_mut().ok_or_else(|| {
        MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            "session does not have a world model",
        )
    })?;
    let mut model = load(&session.repo, reference).map_err(world_model_error)?;
    let result = operation(&mut model)?;
    persist(&session.repo, &reference.relative_path, &model).map_err(world_model_error)?;
    reference.revision = model.revision;
    Ok(result)
}

fn world_model_error(error: medusa_world_model::WorldModelError) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        format!("world model operation failed: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use medusa_core::SessionId;
    use medusa_world_model::create_for_session;
    use std::path::PathBuf;
    use time::OffsetDateTime;

    fn session(repo: PathBuf) -> AgentSession {
        let id = SessionId::new();
        let world_model = create_for_session(&repo, id.as_str(), "fix the defect").ok();
        AgentSession {
            id,
            repo,
            objective: "fix the defect".to_owned(),
            created_at: OffsetDateTime::now_utc(),
            updated_at: OffsetDateTime::now_utc(),
            messages: Vec::new(),
            events: Vec::new(),
            evidence: Vec::new(),
            plan: Vec::new(),
            completed: false,
            turn: 0,
            pending_question: None,
            approval_grants: Vec::new(),
            approval_receipts: Vec::new(),
            rollback_receipts: Vec::new(),
            tool_artifacts: Vec::new(),
            world_model,
            usage: Default::default(),
        }
    }

    #[test]
    fn hypothesis_mutations_update_session_revision() {
        let directory = tempfile::tempdir().expect("repository");
        let mut session = session(directory.path().to_path_buf());
        let initial_revision = session.world_model.as_ref().expect("model").revision;
        let observation =
            record_user_observation(&mut session, "the failing test is deterministic")
                .expect("observation");
        let hypothesis = add_hypothesis(
            &mut session,
            "the failure is caused by deterministic state",
            vec![observation],
        )
        .expect("hypothesis");
        transition_hypothesis(
            &mut session,
            &hypothesis,
            HypothesisStatus::Leading,
            Confidence::Medium,
        )
        .expect("transition");
        assert!(session.world_model.as_ref().expect("model").revision > initial_revision);
    }
}
