//! Runtime access to the canonical behavioral outcome projection.

use std::{path::Path, process::Command};

use medusa_agent::session_browser::replay_events;
pub use medusa_improvement::behavioral_metrics;
pub use medusa_improvement::behavioral_outcome::{
    BEHAVIORAL_OUTCOME_SCHEMA_VERSION, BehavioralComplexityBand, BehavioralModelExecutionV1,
    BehavioralOutcomeV1, BehavioralRiskClass, BehavioralTaskClassificationV1, BehavioralTaskIntent,
    BehavioralTerminalStatus, BehavioralToolExecutionV1, BehavioralWorkspaceMode,
    TASK_CLASSIFICATION_SCHEMA_VERSION, project_behavioral_outcome,
};

use crate::RuntimeError;

pub fn behavioral_outcome(
    repo: &Path,
    session_id: &str,
) -> Result<BehavioralOutcomeV1, RuntimeError> {
    let events = replay_events(repo, session_id, 0).map_err(RuntimeError::agent)?;
    project_behavioral_outcome(
        session_id,
        repository_revision(repo),
        format!("medusa-runtime/{}", env!("CARGO_PKG_VERSION")),
        &events,
    )
    .map_err(RuntimeError::agent)
}

pub fn behavioral_outcome_from_events(
    session_id: &str,
    repository_revision: Option<String>,
    events: &[medusa_protocol::EventEnvelope],
) -> Result<BehavioralOutcomeV1, RuntimeError> {
    project_behavioral_outcome(
        session_id,
        repository_revision,
        format!("medusa-runtime/{}", env!("CARGO_PKG_VERSION")),
        events,
    )
    .map_err(RuntimeError::agent)
}

fn repository_revision(repo: &Path) -> Option<String> {
    Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}
