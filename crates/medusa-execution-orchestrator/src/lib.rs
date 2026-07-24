//! Deterministic, resumable orchestration for autonomous Medusa executions.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use medusa_repository_snapshot::ExecutionManifest;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum ExecutionStage {
    Snapshot,
    Context,
    Memory,
    Plan,
    Workers,
    ReadSetValidation,
    PatchTransaction,
    Verification,
    MemoryConsolidation,
    MemoryWriteback,
    Manifest,
    Complete,
}

impl ExecutionStage {
    pub fn next(self) -> Option<Self> {
        use ExecutionStage::*;
        Some(match self {
            Snapshot => Context,
            Context => Memory,
            Memory => Plan,
            Plan => Workers,
            Workers => ReadSetValidation,
            ReadSetValidation => PatchTransaction,
            PatchTransaction => Verification,
            Verification => MemoryConsolidation,
            MemoryConsolidation => MemoryWriteback,
            MemoryWriteback => Manifest,
            Manifest => Complete,
            Complete => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum FailureDisposition {
    RetrySafe,
    ResumeRequired,
    Terminal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StageFailure {
    pub stage: ExecutionStage,
    pub code: String,
    pub message: String,
    pub disposition: FailureDisposition,
    pub fingerprint: String,
}

impl StageFailure {
    pub fn new(stage: ExecutionStage, code: impl Into<String>, message: impl Into<String>, disposition: FailureDisposition) -> Result<Self, &'static str> {
        let code = code.into();
        let message = message.into();
        if code.trim().is_empty() || message.trim().is_empty() {
            return Err("failure code and message cannot be empty");
        }
        let fingerprint = fingerprint(&(stage, code.as_str(), message.as_str(), disposition));
        Ok(Self { stage, code, message, disposition, fingerprint })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Checkpoint {
    pub execution_id: String,
    pub completed_stage: ExecutionStage,
    pub snapshot_fingerprint: String,
    pub artifact_fingerprints: Vec<String>,
    pub attempt: u32,
    pub fingerprint: String,
}

impl Checkpoint {
    pub fn record(execution_id: impl Into<String>, completed_stage: ExecutionStage, snapshot_fingerprint: impl Into<String>, mut artifact_fingerprints: Vec<String>, attempt: u32) -> Result<Self, &'static str> {
        let execution_id = execution_id.into();
        let snapshot_fingerprint = snapshot_fingerprint.into();
        if execution_id.trim().is_empty() || attempt == 0 {
            return Err("checkpoint requires an execution identifier and non-zero attempt");
        }
        validate_digest(&snapshot_fingerprint)?;
        for value in &artifact_fingerprints { validate_digest(value)?; }
        artifact_fingerprints.sort();
        artifact_fingerprints.dedup();
        let fingerprint = fingerprint(&(execution_id.as_str(), completed_stage, snapshot_fingerprint.as_str(), &artifact_fingerprints, attempt));
        Ok(Self { execution_id, completed_stage, snapshot_fingerprint, artifact_fingerprints, attempt, fingerprint })
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        let rebuilt = Self::record(self.execution_id.clone(), self.completed_stage, self.snapshot_fingerprint.clone(), self.artifact_fingerprints.clone(), self.attempt)?;
        if rebuilt.fingerprint != self.fingerprint { return Err("checkpoint fingerprint does not match its contents"); }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionState {
    pub execution_id: String,
    pub current_stage: ExecutionStage,
    pub snapshot_fingerprint: String,
    pub checkpoints: Vec<Checkpoint>,
    pub failures: Vec<StageFailure>,
    pub attempt: u32,
    pub fingerprint: String,
}

impl ExecutionState {
    pub fn start(execution_id: impl Into<String>, snapshot_fingerprint: impl Into<String>) -> Result<Self, &'static str> {
        let execution_id = execution_id.into();
        let snapshot_fingerprint = snapshot_fingerprint.into();
        if execution_id.trim().is_empty() { return Err("execution identifier cannot be empty"); }
        validate_digest(&snapshot_fingerprint)?;
        let mut state = Self { execution_id, current_stage: ExecutionStage::Snapshot, snapshot_fingerprint, checkpoints: Vec::new(), failures: Vec::new(), attempt: 1, fingerprint: String::new() };
        state.refresh();
        Ok(state)
    }

    pub fn complete_stage(&mut self, stage: ExecutionStage, artifacts: Vec<String>) -> Result<&Checkpoint, &'static str> {
        self.validate()?;
        if stage != self.current_stage || stage == ExecutionStage::Complete { return Err("stage completion is out of order"); }
        let checkpoint = Checkpoint::record(self.execution_id.clone(), stage, self.snapshot_fingerprint.clone(), artifacts, self.attempt)?;
        self.checkpoints.push(checkpoint);
        self.current_stage = stage.next().ok_or("completed execution has no next stage")?;
        self.refresh();
        Ok(self.checkpoints.last().expect("checkpoint was appended"))
    }

    pub fn record_failure(&mut self, failure: StageFailure) -> Result<(), &'static str> {
        self.validate()?;
        if failure.stage != self.current_stage { return Err("failure stage does not match current execution stage"); }
        if failure.disposition == FailureDisposition::RetrySafe { self.attempt = self.attempt.saturating_add(1); }
        self.failures.push(failure);
        self.refresh();
        Ok(())
    }

    pub fn resume(checkpoint: Checkpoint) -> Result<Self, &'static str> {
        checkpoint.validate()?;
        let next = checkpoint.completed_stage.next().ok_or("cannot resume after completion")?;
        let mut state = Self {
            execution_id: checkpoint.execution_id.clone(),
            current_stage: next,
            snapshot_fingerprint: checkpoint.snapshot_fingerprint.clone(),
            attempt: checkpoint.attempt.saturating_add(1),
            checkpoints: vec![checkpoint],
            failures: Vec::new(),
            fingerprint: String::new(),
        };
        state.refresh();
        Ok(state)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        validate_digest(&self.snapshot_fingerprint)?;
        for checkpoint in &self.checkpoints { checkpoint.validate()?; }
        let expected = fingerprint(&(self.execution_id.as_str(), self.current_stage, self.snapshot_fingerprint.as_str(), &self.checkpoints, &self.failures, self.attempt));
        if expected != self.fingerprint { return Err("execution state fingerprint does not match its contents"); }
        Ok(())
    }

    fn refresh(&mut self) {
        self.fingerprint = fingerprint(&(self.execution_id.as_str(), self.current_stage, self.snapshot_fingerprint.as_str(), &self.checkpoints, &self.failures, self.attempt));
    }
}

#[allow(clippy::too_many_arguments)]
pub fn finalize_manifest(state: &ExecutionState, prompt: &str, context: &str, memory: &str, tool_outputs: &[String], transactions: &[String], result: &str) -> Result<ExecutionManifest, &'static str> {
    state.validate()?;
    if state.current_stage != ExecutionStage::Manifest && state.current_stage != ExecutionStage::Complete {
        return Err("execution cannot be finalized before the manifest stage");
    }
    ExecutionManifest::record(
        state.execution_id.clone(),
        state.snapshot_fingerprint.clone(),
        fingerprint_bytes(prompt.as_bytes()),
        fingerprint_bytes(context.as_bytes()),
        fingerprint_bytes(memory.as_bytes()),
        tool_outputs.iter().map(|value| fingerprint_bytes(value.as_bytes())).collect(),
        transactions.to_vec(),
        fingerprint_bytes(result.as_bytes()),
    )
}

fn validate_digest(value: &str) -> Result<(), &'static str> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) { return Err("fingerprint must be a SHA-256 hex digest"); }
    Ok(())
}

fn fingerprint<T: Serialize>(value: &T) -> String {
    fingerprint_bytes(&serde_json::to_vec(value).expect("serializing orchestration data cannot fail"))
}

fn fingerprint_bytes(bytes: &[u8]) -> String { hex::encode(Sha256::digest(bytes)) }

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(value: &str) -> String { fingerprint_bytes(value.as_bytes()) }

    #[test]
    fn stages_advance_in_order_and_checkpoint() {
        let mut state = ExecutionState::start("run-1", digest("snapshot")).unwrap();
        state.complete_stage(ExecutionStage::Snapshot, vec![digest("snapshot-artifact")]).unwrap();
        assert_eq!(state.current_stage, ExecutionStage::Context);
        assert_eq!(state.checkpoints.len(), 1);
        state.validate().unwrap();
    }

    #[test]
    fn out_of_order_stage_is_rejected() {
        let mut state = ExecutionState::start("run-1", digest("snapshot")).unwrap();
        assert!(state.complete_stage(ExecutionStage::Plan, vec![]).is_err());
    }

    #[test]
    fn retry_safe_failure_increments_attempt() {
        let mut state = ExecutionState::start("run-1", digest("snapshot")).unwrap();
        let failure = StageFailure::new(ExecutionStage::Snapshot, "temporary-io", "retry", FailureDisposition::RetrySafe).unwrap();
        state.record_failure(failure).unwrap();
        assert_eq!(state.attempt, 2);
    }

    #[test]
    fn resumes_from_next_stage() {
        let checkpoint = Checkpoint::record("run-1", ExecutionStage::Plan, digest("snapshot"), vec![digest("plan")], 1).unwrap();
        let state = ExecutionState::resume(checkpoint).unwrap();
        assert_eq!(state.current_stage, ExecutionStage::Workers);
        assert_eq!(state.attempt, 2);
    }

    #[test]
    fn manifest_requires_manifest_stage() {
        let state = ExecutionState::start("run-1", digest("snapshot")).unwrap();
        assert!(finalize_manifest(&state, "p", "c", "m", &[], &[], "r").is_err());
    }
}
