use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use ulid::Ulid;

pub const WORLD_MODEL_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum WorldModelError {
    #[error("world model I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("world model serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported world model schema version {0}")]
    UnsupportedSchema(u32),
    #[error("evidence link references missing observation {0}")]
    MissingObservation(String),
    #[error("invalid world model transition: {0}")]
    InvalidTransition(String),
}

pub type WorldModelResult<T> = Result<T, WorldModelError>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorldModelRef {
    pub relative_path: PathBuf,
    pub revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkspaceModel {
    pub schema_version: u32,
    pub objective: ObjectiveModel,
    #[serde(default)]
    pub observations: Vec<Observation>,
    #[serde(default)]
    pub hypotheses: Vec<Hypothesis>,
    #[serde(default)]
    pub experiments: Vec<Experiment>,
    #[serde(default)]
    pub invariants: Vec<Invariant>,
    pub revision: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl WorkspaceModel {
    #[must_use]
    pub fn new(objective: impl Into<String>) -> Self {
        let objective = objective.into();
        Self {
            schema_version: WORLD_MODEL_SCHEMA_VERSION,
            objective: ObjectiveModel {
                original_request: objective.clone(),
                normalized_goal: objective,
                acceptance_criteria: Vec::new(),
                constraints: Vec::new(),
            },
            observations: Vec::new(),
            hypotheses: Vec::new(),
            experiments: Vec::new(),
            invariants: Vec::new(),
            revision: 0,
            updated_at: OffsetDateTime::now_utc(),
        }
    }

    pub fn record_observation(
        &mut self,
        source: ObservationSource,
        statement: impl Into<String>,
    ) -> String {
        let id = format!("obs_{}", Ulid::new());
        self.observations.push(Observation {
            id: id.clone(),
            source,
            statement: statement.into(),
            captured_at: OffsetDateTime::now_utc(),
            invalidated_at: None,
        });
        self.touch();
        id
    }

    pub fn add_hypothesis(
        &mut self,
        statement: impl Into<String>,
        supporting_observations: Vec<String>,
    ) -> WorldModelResult<String> {
        self.validate_observation_ids(&supporting_observations)?;
        let id = format!("hyp_{}", Ulid::new());
        self.hypotheses.push(Hypothesis {
            id: id.clone(),
            statement: statement.into(),
            status: HypothesisStatus::Candidate,
            confidence: Confidence::Low,
            supporting_observations,
            contradicting_observations: Vec::new(),
        });
        self.touch();
        Ok(id)
    }

    pub fn transition_hypothesis(
        &mut self,
        id: &str,
        status: HypothesisStatus,
        confidence: Confidence,
    ) -> WorldModelResult<()> {
        let hypothesis = self
            .hypotheses
            .iter_mut()
            .find(|hypothesis| hypothesis.id == id)
            .ok_or_else(|| WorldModelError::InvalidTransition(format!("unknown hypothesis {id}")))?;
        if hypothesis.status == HypothesisStatus::Refuted
            && matches!(status, HypothesisStatus::Leading | HypothesisStatus::Supported)
        {
            return Err(WorldModelError::InvalidTransition(
                "refuted hypotheses require new evidence before promotion".to_owned(),
            ));
        }
        hypothesis.status = status;
        hypothesis.confidence = confidence;
        self.touch();
        Ok(())
    }

    pub fn add_experiment(&mut self, experiment: Experiment) -> WorldModelResult<()> {
        for prediction in &experiment.predictions {
            if !self
                .hypotheses
                .iter()
                .any(|hypothesis| hypothesis.id == prediction.hypothesis_id)
            {
                return Err(WorldModelError::InvalidTransition(format!(
                    "prediction references missing hypothesis {}",
                    prediction.hypothesis_id
                )));
            }
        }
        self.experiments.push(experiment);
        self.touch();
        Ok(())
    }

    pub fn validate(&self) -> WorldModelResult<()> {
        if self.schema_version != WORLD_MODEL_SCHEMA_VERSION {
            return Err(WorldModelError::UnsupportedSchema(self.schema_version));
        }
        for hypothesis in &self.hypotheses {
            self.validate_observation_ids(&hypothesis.supporting_observations)?;
            self.validate_observation_ids(&hypothesis.contradicting_observations)?;
        }
        Ok(())
    }

    fn validate_observation_ids(&self, ids: &[String]) -> WorldModelResult<()> {
        for id in ids {
            if !self.observations.iter().any(|observation| &observation.id == id) {
                return Err(WorldModelError::MissingObservation(id.clone()));
            }
        }
        Ok(())
    }

    fn touch(&mut self) {
        self.revision = self.revision.saturating_add(1);
        self.updated_at = OffsetDateTime::now_utc();
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObjectiveModel {
    pub original_request: String,
    pub normalized_goal: String,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Observation {
    pub id: String,
    pub source: ObservationSource,
    pub statement: String,
    #[serde(with = "time::serde::rfc3339")]
    pub captured_at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub invalidated_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ObservationSource {
    FileRead { path: PathBuf, content_hash: Option<String> },
    SearchResult { query: String },
    ShellCommand { command: String, exit_code: i32 },
    TestRun { command: String, exit_code: i32 },
    UserStatement,
    Derived,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Hypothesis {
    pub id: String,
    pub statement: String,
    pub status: HypothesisStatus,
    pub confidence: Confidence,
    #[serde(default)]
    pub supporting_observations: Vec<String>,
    #[serde(default)]
    pub contradicting_observations: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HypothesisStatus {
    Candidate,
    Leading,
    Supported,
    Refuted,
    Superseded,
    Unresolved,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
    Verified,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Experiment {
    pub id: String,
    pub question: String,
    pub action: ExperimentAction,
    #[serde(default)]
    pub predictions: Vec<Prediction>,
    pub status: ExperimentStatus,
    pub result: Option<ExperimentResult>,
}

impl Experiment {
    #[must_use]
    pub fn new(question: impl Into<String>, action: ExperimentAction) -> Self {
        Self {
            id: format!("exp_{}", Ulid::new()),
            question: question.into(),
            action,
            predictions: Vec::new(),
            status: ExperimentStatus::Proposed,
            result: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExperimentAction {
    ReadFile { path: PathBuf },
    SearchText { query: String, scope: PathBuf },
    RunCommand { command: String },
    RunTest { command: String },
    InspectGitHistory { path: Option<PathBuf> },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Prediction {
    pub hypothesis_id: String,
    pub expected_observation: String,
    pub outcome: Option<PredictionOutcome>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PredictionOutcome {
    Confirmed { observation_ids: Vec<String> },
    Contradicted { observation_ids: Vec<String> },
    Inconclusive { reason: String },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentStatus {
    Proposed,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExperimentResult {
    pub summary: String,
    #[serde(default)]
    pub observation_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Invariant {
    pub id: String,
    pub statement: String,
    pub verification: String,
    pub status: InvariantStatus,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InvariantStatus {
    Unknown,
    Preserved,
    Violated,
}

pub fn model_relative_path(session_id: &str) -> PathBuf {
    PathBuf::from(".medusa")
        .join("world-models")
        .join(session_id)
        .join("model.json")
}

pub fn create_for_session(
    repo: &Path,
    session_id: &str,
    objective: impl Into<String>,
) -> WorldModelResult<WorldModelRef> {
    let relative_path = model_relative_path(session_id);
    let model = WorkspaceModel::new(objective);
    persist(repo, &relative_path, &model)?;
    Ok(WorldModelRef {
        relative_path,
        revision: model.revision,
    })
}

pub fn load(repo: &Path, reference: &WorldModelRef) -> WorldModelResult<WorkspaceModel> {
    let model: WorkspaceModel = serde_json::from_slice(&fs::read(repo.join(&reference.relative_path))?)?;
    model.validate()?;
    Ok(model)
}

pub fn persist(
    repo: &Path,
    relative_path: &Path,
    model: &WorkspaceModel,
) -> WorldModelResult<()> {
    model.validate()?;
    let path = repo.join(relative_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(model)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_round_trips_and_preserves_evidence_links() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut model = WorkspaceModel::new("diagnose cancellation failure");
        let observation = model.record_observation(
            ObservationSource::TestRun {
                command: "cargo test cancellation".to_owned(),
                exit_code: 1,
            },
            "the cancellation test times out",
        );
        model
            .add_hypothesis("the child process is never signalled", vec![observation])
            .expect("hypothesis");
        let reference = create_for_session(directory.path(), "session-1", "placeholder")
            .expect("create reference");
        persist(directory.path(), &reference.relative_path, &model).expect("persist model");
        let restored = load(directory.path(), &reference).expect("load model");
        assert_eq!(restored, model);
    }

    #[test]
    fn hypothesis_rejects_fabricated_observation_ids() {
        let mut model = WorkspaceModel::new("fix a bug");
        let error = model
            .add_hypothesis("candidate", vec!["obs_missing".to_owned()])
            .expect_err("missing observation must fail");
        assert!(matches!(error, WorldModelError::MissingObservation(_)));
    }

    #[test]
    fn refuted_hypothesis_cannot_be_promoted_without_new_evidence() {
        let mut model = WorkspaceModel::new("fix a bug");
        let observation = model.record_observation(ObservationSource::UserStatement, "failure");
        let hypothesis = model
            .add_hypothesis("candidate", vec![observation])
            .expect("hypothesis");
        model
            .transition_hypothesis(&hypothesis, HypothesisStatus::Refuted, Confidence::Low)
            .expect("refute");
        assert!(
            model
                .transition_hypothesis(
                    &hypothesis,
                    HypothesisStatus::Leading,
                    Confidence::High,
                )
                .is_err()
        );
    }
}
