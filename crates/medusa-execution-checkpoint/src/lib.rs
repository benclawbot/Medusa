use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionEvent {
    pub sequence: u64,
    pub kind: String,
    pub payload_fingerprint: String,
    pub previous_event_fingerprint: Option<String>,
    pub fingerprint: String,
}

impl ExecutionEvent {
    pub fn new(
        sequence: u64,
        kind: impl Into<String>,
        payload_fingerprint: impl Into<String>,
        previous_event_fingerprint: Option<String>,
    ) -> Result<Self, CheckpointError> {
        let mut event = Self {
            sequence,
            kind: kind.into(),
            payload_fingerprint: payload_fingerprint.into(),
            previous_event_fingerprint,
            fingerprint: String::new(),
        };
        event.validate_fields()?;
        event.fingerprint = event.calculate_fingerprint();
        Ok(event)
    }

    fn validate_fields(&self) -> Result<(), CheckpointError> {
        if self.kind.trim().is_empty() {
            return Err(CheckpointError::EmptyEventKind);
        }
        validate_sha256(&self.payload_fingerprint)?;
        if let Some(previous) = &self.previous_event_fingerprint {
            validate_sha256(previous)?;
        }
        Ok(())
    }

    fn calculate_fingerprint(&self) -> String {
        hash_json(&(
            self.sequence,
            &self.kind,
            &self.payload_fingerprint,
            &self.previous_event_fingerprint,
        ))
    }

    pub fn verify(&self) -> Result<(), CheckpointError> {
        self.validate_fields()?;
        validate_sha256(&self.fingerprint)?;
        if self.fingerprint != self.calculate_fingerprint() {
            return Err(CheckpointError::FingerprintMismatch);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionCheckpoint {
    pub execution_id: String,
    pub sequence: u64,
    pub supervisor_fingerprint: String,
    pub repository_snapshot_fingerprint: String,
    pub last_event_fingerprint: Option<String>,
    pub subsystem_fingerprints: BTreeMap<String, String>,
    pub fingerprint: String,
}

impl ExecutionCheckpoint {
    pub fn new(
        execution_id: impl Into<String>,
        sequence: u64,
        supervisor_fingerprint: impl Into<String>,
        repository_snapshot_fingerprint: impl Into<String>,
        last_event_fingerprint: Option<String>,
        subsystem_fingerprints: BTreeMap<String, String>,
    ) -> Result<Self, CheckpointError> {
        let mut checkpoint = Self {
            execution_id: execution_id.into(),
            sequence,
            supervisor_fingerprint: supervisor_fingerprint.into(),
            repository_snapshot_fingerprint: repository_snapshot_fingerprint.into(),
            last_event_fingerprint,
            subsystem_fingerprints,
            fingerprint: String::new(),
        };
        checkpoint.validate_fields()?;
        checkpoint.fingerprint = checkpoint.calculate_fingerprint();
        Ok(checkpoint)
    }

    fn validate_fields(&self) -> Result<(), CheckpointError> {
        if self.execution_id.trim().is_empty() {
            return Err(CheckpointError::EmptyExecutionId);
        }
        validate_sha256(&self.supervisor_fingerprint)?;
        validate_sha256(&self.repository_snapshot_fingerprint)?;
        if let Some(value) = &self.last_event_fingerprint {
            validate_sha256(value)?;
        }
        for (name, fingerprint) in &self.subsystem_fingerprints {
            if name.trim().is_empty() {
                return Err(CheckpointError::EmptySubsystemName);
            }
            validate_sha256(fingerprint)?;
        }
        Ok(())
    }

    fn calculate_fingerprint(&self) -> String {
        hash_json(&(
            &self.execution_id,
            self.sequence,
            &self.supervisor_fingerprint,
            &self.repository_snapshot_fingerprint,
            &self.last_event_fingerprint,
            &self.subsystem_fingerprints,
        ))
    }

    pub fn verify(&self) -> Result<(), CheckpointError> {
        self.validate_fields()?;
        validate_sha256(&self.fingerprint)?;
        if self.fingerprint != self.calculate_fingerprint() {
            return Err(CheckpointError::FingerprintMismatch);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionLog {
    pub execution_id: String,
    pub events: Vec<ExecutionEvent>,
    pub checkpoints: Vec<ExecutionCheckpoint>,
}

impl ExecutionLog {
    pub fn new(execution_id: impl Into<String>) -> Result<Self, CheckpointError> {
        let execution_id = execution_id.into();
        if execution_id.trim().is_empty() {
            return Err(CheckpointError::EmptyExecutionId);
        }
        Ok(Self {
            execution_id,
            events: Vec::new(),
            checkpoints: Vec::new(),
        })
    }

    pub fn append_event(
        &mut self,
        kind: impl Into<String>,
        payload_fingerprint: impl Into<String>,
    ) -> Result<&ExecutionEvent, CheckpointError> {
        let sequence = self.events.len() as u64 + 1;
        let previous = self.events.last().map(|event| event.fingerprint.clone());
        let event = ExecutionEvent::new(sequence, kind, payload_fingerprint, previous)?;
        self.events.push(event);
        self.events.last().ok_or(CheckpointError::EventAppendFailed)
    }

    pub fn add_checkpoint(
        &mut self,
        checkpoint: ExecutionCheckpoint,
    ) -> Result<(), CheckpointError> {
        checkpoint.verify()?;
        if checkpoint.execution_id != self.execution_id {
            return Err(CheckpointError::ExecutionIdMismatch);
        }
        if checkpoint.sequence > self.events.len() as u64 {
            return Err(CheckpointError::CheckpointAheadOfLog);
        }
        let expected_last = if checkpoint.sequence == 0 {
            None
        } else {
            Some(
                self.events[(checkpoint.sequence - 1) as usize]
                    .fingerprint
                    .clone(),
            )
        };
        if checkpoint.last_event_fingerprint != expected_last {
            return Err(CheckpointError::CheckpointEventMismatch);
        }
        if self
            .checkpoints
            .last()
            .is_some_and(|last| checkpoint.sequence <= last.sequence)
        {
            return Err(CheckpointError::NonMonotonicCheckpoint);
        }
        self.checkpoints.push(checkpoint);
        Ok(())
    }

    pub fn latest_checkpoint(&self) -> Option<&ExecutionCheckpoint> {
        self.checkpoints.last()
    }

    pub fn replay_tail(&self) -> Result<&[ExecutionEvent], CheckpointError> {
        self.verify()?;
        let start = self
            .latest_checkpoint()
            .map_or(0, |checkpoint| checkpoint.sequence as usize);
        Ok(&self.events[start..])
    }

    pub fn verify(&self) -> Result<(), CheckpointError> {
        if self.execution_id.trim().is_empty() {
            return Err(CheckpointError::EmptyExecutionId);
        }
        let mut previous: Option<&str> = None;
        for (index, event) in self.events.iter().enumerate() {
            event.verify()?;
            if event.sequence != index as u64 + 1 {
                return Err(CheckpointError::NonContiguousEventSequence);
            }
            if event.previous_event_fingerprint.as_deref() != previous {
                return Err(CheckpointError::BrokenEventChain);
            }
            previous = Some(&event.fingerprint);
        }
        let mut last_sequence = None;
        for checkpoint in &self.checkpoints {
            checkpoint.verify()?;
            if checkpoint.execution_id != self.execution_id {
                return Err(CheckpointError::ExecutionIdMismatch);
            }
            if checkpoint.sequence > self.events.len() as u64 {
                return Err(CheckpointError::CheckpointAheadOfLog);
            }
            if last_sequence.is_some_and(|last| checkpoint.sequence <= last) {
                return Err(CheckpointError::NonMonotonicCheckpoint);
            }
            let expected = if checkpoint.sequence == 0 {
                None
            } else {
                Some(
                    self.events[(checkpoint.sequence - 1) as usize]
                        .fingerprint
                        .as_str(),
                )
            };
            if checkpoint.last_event_fingerprint.as_deref() != expected {
                return Err(CheckpointError::CheckpointEventMismatch);
            }
            last_sequence = Some(checkpoint.sequence);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactProvenance {
    UserRequirement,
    RepositoryFact,
    VerificationEvidence,
    AgentInference,
    ArchitecturePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningState {
    Candidate,
    Verified,
    Canonical,
    Invalidated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LearningRecord {
    pub learning_id: String,
    pub statement: String,
    pub provenance: FactProvenance,
    pub state: LearningState,
    pub evidence_fingerprint: Option<String>,
}

impl LearningRecord {
    pub fn verify_for_injection(&self) -> Result<(), CheckpointError> {
        if self.learning_id.trim().is_empty() || self.statement.trim().is_empty() {
            return Err(CheckpointError::InvalidLearning);
        }
        if !matches!(
            self.state,
            LearningState::Verified | LearningState::Canonical
        ) {
            return Err(CheckpointError::UnverifiedLearningInjection);
        }
        let evidence = self
            .evidence_fingerprint
            .as_deref()
            .ok_or(CheckpointError::MissingLearningEvidence)?;
        validate_sha256(evidence)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryHypothesis {
    pub failure_fingerprint: String,
    pub previous_capsule_fingerprint: String,
    pub previous_hypothesis: Option<String>,
    pub disproving_evidence: Vec<String>,
    pub new_hypothesis: String,
    pub changed_strategy: String,
    pub environment_fingerprint: String,
}

impl RetryHypothesis {
    pub fn verify(&self) -> Result<(), CheckpointError> {
        validate_sha256(&self.failure_fingerprint)?;
        validate_sha256(&self.previous_capsule_fingerprint)?;
        validate_sha256(&self.environment_fingerprint)?;
        if self.new_hypothesis.trim().is_empty() || self.changed_strategy.trim().is_empty() {
            return Err(CheckpointError::InvalidRetryHypothesis);
        }
        for evidence in &self.disproving_evidence {
            validate_sha256(evidence)?;
        }
        Ok(())
    }

    fn meaningfully_differs_from(&self, previous: &Self) -> bool {
        self.new_hypothesis != previous.new_hypothesis
            || self.changed_strategy != previous.changed_strategy
            || self.environment_fingerprint != previous.environment_fingerprint
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepCapsule {
    pub capsule_id: String,
    pub execution_id: String,
    pub plan_id: String,
    pub plan_revision: u64,
    pub step_id: String,
    pub objective: String,
    pub acceptance_criteria: Vec<String>,
    pub allowed_paths: Vec<String>,
    pub allowed_tools: Vec<String>,
    pub relevant_files: Vec<String>,
    pub verified_learnings: Vec<LearningRecord>,
    pub failure_fingerprint: Option<String>,
    pub retry_hypothesis: Option<RetryHypothesis>,
    pub authority_fingerprint: String,
    pub fresh_context: bool,
    pub fingerprint: String,
}

impl StepCapsule {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        capsule_id: impl Into<String>,
        execution_id: impl Into<String>,
        plan_id: impl Into<String>,
        plan_revision: u64,
        step_id: impl Into<String>,
        objective: impl Into<String>,
        acceptance_criteria: Vec<String>,
        allowed_paths: Vec<String>,
        allowed_tools: Vec<String>,
        relevant_files: Vec<String>,
        verified_learnings: Vec<LearningRecord>,
        failure_fingerprint: Option<String>,
        retry_hypothesis: Option<RetryHypothesis>,
        authority_fingerprint: impl Into<String>,
    ) -> Result<Self, CheckpointError> {
        let mut capsule = Self {
            capsule_id: capsule_id.into(),
            execution_id: execution_id.into(),
            plan_id: plan_id.into(),
            plan_revision,
            step_id: step_id.into(),
            objective: objective.into(),
            acceptance_criteria,
            allowed_paths,
            allowed_tools,
            relevant_files,
            verified_learnings,
            failure_fingerprint,
            retry_hypothesis,
            authority_fingerprint: authority_fingerprint.into(),
            fresh_context: true,
            fingerprint: String::new(),
        };
        capsule.validate_fields()?;
        capsule.fingerprint = capsule.calculate_fingerprint();
        Ok(capsule)
    }

    fn validate_fields(&self) -> Result<(), CheckpointError> {
        if self.capsule_id.trim().is_empty()
            || self.execution_id.trim().is_empty()
            || self.plan_id.trim().is_empty()
            || self.step_id.trim().is_empty()
        {
            return Err(CheckpointError::InvalidStepCapsuleIdentity);
        }
        if self.plan_revision == 0 {
            return Err(CheckpointError::InvalidPlanRevision);
        }
        if self.objective.trim().is_empty() || self.acceptance_criteria.is_empty() {
            return Err(CheckpointError::IncompleteStepCapsule);
        }
        if self
            .acceptance_criteria
            .iter()
            .any(|criterion| criterion.trim().is_empty())
        {
            return Err(CheckpointError::IncompleteStepCapsule);
        }
        if !self.fresh_context {
            return Err(CheckpointError::FreshContextRequired);
        }
        validate_sha256(&self.authority_fingerprint)?;
        if let Some(failure) = &self.failure_fingerprint {
            validate_sha256(failure)?;
        }
        for learning in &self.verified_learnings {
            learning.verify_for_injection()?;
        }
        if let Some(hypothesis) = &self.retry_hypothesis {
            hypothesis.verify()?;
            if self.failure_fingerprint.as_deref() != Some(hypothesis.failure_fingerprint.as_str())
            {
                return Err(CheckpointError::RetryFailureMismatch);
            }
        }
        Ok(())
    }

    fn calculate_fingerprint(&self) -> String {
        hash_json(&(
            &self.capsule_id,
            &self.execution_id,
            &self.plan_id,
            self.plan_revision,
            &self.step_id,
            &self.objective,
            &self.acceptance_criteria,
            &self.allowed_paths,
            &self.allowed_tools,
            &self.relevant_files,
            &self.verified_learnings,
            &self.failure_fingerprint,
            &self.retry_hypothesis,
            &self.authority_fingerprint,
            self.fresh_context,
        ))
    }

    pub fn verify(&self) -> Result<(), CheckpointError> {
        self.validate_fields()?;
        validate_sha256(&self.fingerprint)?;
        if self.fingerprint != self.calculate_fingerprint() {
            return Err(CheckpointError::FingerprintMismatch);
        }
        Ok(())
    }

    pub fn verify_retry_from(&self, previous: &Self) -> Result<(), CheckpointError> {
        previous.verify()?;
        self.verify()?;
        if self.execution_id != previous.execution_id
            || self.plan_id != previous.plan_id
            || self.plan_revision != previous.plan_revision
            || self.step_id != previous.step_id
        {
            return Err(CheckpointError::RetryScopeMismatch);
        }
        if self.authority_fingerprint != previous.authority_fingerprint
            || self.objective != previous.objective
            || self.acceptance_criteria != previous.acceptance_criteria
            || self.allowed_paths != previous.allowed_paths
            || self.allowed_tools != previous.allowed_tools
        {
            return Err(CheckpointError::RetryAuthorityMismatch);
        }
        if self.capsule_id == previous.capsule_id || self.fingerprint == previous.fingerprint {
            return Err(CheckpointError::FreshContextRequired);
        }
        let hypothesis = self
            .retry_hypothesis
            .as_ref()
            .ok_or(CheckpointError::MissingRetryHypothesis)?;
        if hypothesis.previous_capsule_fingerprint != previous.fingerprint {
            return Err(CheckpointError::RetryLineageMismatch);
        }
        if let Some(previous_hypothesis) = &previous.retry_hypothesis {
            if !hypothesis.meaningfully_differs_from(previous_hypothesis) {
                return Err(CheckpointError::UnchangedRetryStrategy);
            }
        } else if hypothesis.previous_hypothesis.is_none()
            && hypothesis.disproving_evidence.is_empty()
        {
            return Err(CheckpointError::RetryDeltaRequired);
        }
        Ok(())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CheckpointError {
    #[error("execution id must not be empty")]
    EmptyExecutionId,
    #[error("event kind must not be empty")]
    EmptyEventKind,
    #[error("event append did not persist")]
    EventAppendFailed,
    #[error("subsystem name must not be empty")]
    EmptySubsystemName,
    #[error("fingerprint must be a lowercase 64-character SHA-256 hex digest")]
    InvalidFingerprint,
    #[error("fingerprint does not match canonical state")]
    FingerprintMismatch,
    #[error("event sequence is not contiguous")]
    NonContiguousEventSequence,
    #[error("event hash chain is broken")]
    BrokenEventChain,
    #[error("checkpoint belongs to a different execution")]
    ExecutionIdMismatch,
    #[error("checkpoint sequence is ahead of the event log")]
    CheckpointAheadOfLog,
    #[error("checkpoint does not reference the event at its sequence")]
    CheckpointEventMismatch,
    #[error("checkpoint sequences must increase monotonically")]
    NonMonotonicCheckpoint,
    #[error("learning id and statement must not be empty")]
    InvalidLearning,
    #[error("only verified or canonical learnings may enter a fresh execution context")]
    UnverifiedLearningInjection,
    #[error("verified learnings require evidence provenance")]
    MissingLearningEvidence,
    #[error("retry hypothesis is incomplete")]
    InvalidRetryHypothesis,
    #[error("step capsule identity fields must not be empty")]
    InvalidStepCapsuleIdentity,
    #[error("plan revision must start at one")]
    InvalidPlanRevision,
    #[error("step capsule requires an objective and acceptance criteria")]
    IncompleteStepCapsule,
    #[error("step execution and retries require a newly constructed context capsule")]
    FreshContextRequired,
    #[error("retry hypothesis must reference the capsule failure")]
    RetryFailureMismatch,
    #[error("retry capsule changed execution, plan, revision, or step identity")]
    RetryScopeMismatch,
    #[error("retry capsule changed objective, acceptance criteria, tools, paths, or authority")]
    RetryAuthorityMismatch,
    #[error("retry capsule does not name the immediately preceding capsule fingerprint")]
    RetryLineageMismatch,
    #[error("retry requires an explicit hypothesis")]
    MissingRetryHypothesis,
    #[error("first retry must record a prior hypothesis or disproving evidence")]
    RetryDeltaRequired,
    #[error("retry strategy is unchanged from the previous attempt")]
    UnchangedRetryStrategy,
}

fn validate_sha256(value: &str) -> Result<(), CheckpointError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(CheckpointError::InvalidFingerprint);
    }
    Ok(())
}

fn hash_json<T: Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(label: &str) -> String {
        hex::encode(Sha256::digest(label.as_bytes()))
    }

    fn checkpoint(log: &ExecutionLog, sequence: u64) -> ExecutionCheckpoint {
        ExecutionCheckpoint::new(
            log.execution_id.clone(),
            sequence,
            digest("supervisor"),
            digest("snapshot"),
            if sequence == 0 {
                None
            } else {
                Some(log.events[(sequence - 1) as usize].fingerprint.clone())
            },
            BTreeMap::from([("leases".into(), digest("leases"))]),
        )
        .unwrap()
    }

    fn learning(state: LearningState) -> LearningRecord {
        LearningRecord {
            learning_id: "learning-1".into(),
            statement: "the repository requires locked tests".into(),
            provenance: FactProvenance::VerificationEvidence,
            state,
            evidence_fingerprint: Some(digest("test-receipt")),
        }
    }

    fn capsule(
        capsule_id: &str,
        failure: Option<&str>,
        retry_hypothesis: Option<RetryHypothesis>,
    ) -> Result<StepCapsule, CheckpointError> {
        StepCapsule::new(
            capsule_id,
            "run-1",
            "plan-1",
            1,
            "implement",
            "Implement the bounded change",
            vec!["targeted tests pass".into()],
            vec!["crates/medusa-execution-checkpoint".into()],
            vec!["read_file".into(), "write_file".into()],
            vec!["src/lib.rs".into()],
            vec![learning(LearningState::Verified)],
            failure.map(digest),
            retry_hypothesis,
            digest("authority"),
        )
    }

    #[test]
    fn appends_hash_chained_events_and_verifies() {
        let mut log = ExecutionLog::new("run-1").unwrap();
        log.append_event("scheduled", digest("schedule")).unwrap();
        log.append_event("leased", digest("leases")).unwrap();
        assert_eq!(
            log.events[1].previous_event_fingerprint.as_deref(),
            Some(log.events[0].fingerprint.as_str())
        );
        log.verify().unwrap();
    }

    #[test]
    fn checkpoint_replay_returns_only_uncheckpointed_tail() {
        let mut log = ExecutionLog::new("run-1").unwrap();
        log.append_event("scheduled", digest("schedule")).unwrap();
        log.append_event("leased", digest("leases")).unwrap();
        log.add_checkpoint(checkpoint(&log, 1)).unwrap();
        assert_eq!(log.replay_tail().unwrap(), &log.events[1..]);
    }

    #[test]
    fn rejects_checkpoint_that_does_not_match_event_chain() {
        let mut log = ExecutionLog::new("run-1").unwrap();
        log.append_event("scheduled", digest("schedule")).unwrap();
        let mut value = checkpoint(&log, 1);
        value.last_event_fingerprint = Some(digest("other"));
        value.fingerprint = value.calculate_fingerprint();
        assert_eq!(
            log.add_checkpoint(value),
            Err(CheckpointError::CheckpointEventMismatch)
        );
    }

    #[test]
    fn detects_event_tampering() {
        let mut log = ExecutionLog::new("run-1").unwrap();
        log.append_event("scheduled", digest("schedule")).unwrap();
        log.events[0].kind = "changed".into();
        assert_eq!(log.verify(), Err(CheckpointError::FingerprintMismatch));
    }

    #[test]
    fn rejects_non_monotonic_checkpoints() {
        let mut log = ExecutionLog::new("run-1").unwrap();
        log.append_event("scheduled", digest("schedule")).unwrap();
        log.add_checkpoint(checkpoint(&log, 1)).unwrap();
        assert_eq!(
            log.add_checkpoint(checkpoint(&log, 1)),
            Err(CheckpointError::NonMonotonicCheckpoint)
        );
    }

    #[test]
    fn capsule_rejects_unverified_learning_injection() {
        let result = StepCapsule::new(
            "capsule-1",
            "run-1",
            "plan-1",
            1,
            "implement",
            "Implement the bounded change",
            vec!["tests pass".into()],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![learning(LearningState::Candidate)],
            None,
            None,
            digest("authority"),
        );
        assert_eq!(result, Err(CheckpointError::UnverifiedLearningInjection));
    }

    #[test]
    fn retry_requires_a_new_capsule_and_changed_strategy() {
        let previous_hypothesis = RetryHypothesis {
            failure_fingerprint: digest("failure"),
            previous_capsule_fingerprint: digest("older-capsule"),
            previous_hypothesis: Some("dependency mismatch".into()),
            disproving_evidence: vec![digest("compiler-output")],
            new_hypothesis: "trait contract changed".into(),
            changed_strategy: "update the implementation against the trait".into(),
            environment_fingerprint: digest("env"),
        };
        let previous = capsule("capsule-1", Some("failure"), Some(previous_hypothesis)).unwrap();
        let next_hypothesis = RetryHypothesis {
            failure_fingerprint: digest("failure"),
            previous_capsule_fingerprint: previous.fingerprint.clone(),
            previous_hypothesis: Some("trait contract changed".into()),
            disproving_evidence: vec![digest("targeted-test")],
            new_hypothesis: "fixture still uses the old contract".into(),
            changed_strategy: "update the fixture and rerun the targeted test".into(),
            environment_fingerprint: digest("env"),
        };
        let next = capsule("capsule-2", Some("failure"), Some(next_hypothesis)).unwrap();
        next.verify_retry_from(&previous).unwrap();
    }

    #[test]
    fn retry_rejects_identical_strategy() {
        let previous_hypothesis = RetryHypothesis {
            failure_fingerprint: digest("failure"),
            previous_capsule_fingerprint: digest("older-capsule"),
            previous_hypothesis: Some("dependency mismatch".into()),
            disproving_evidence: vec![digest("compiler-output")],
            new_hypothesis: "trait contract changed".into(),
            changed_strategy: "update the implementation against the trait".into(),
            environment_fingerprint: digest("env"),
        };
        let previous = capsule(
            "capsule-1",
            Some("failure"),
            Some(previous_hypothesis.clone()),
        )
        .unwrap();
        let hypothesis = RetryHypothesis {
            previous_capsule_fingerprint: previous.fingerprint.clone(),
            ..previous_hypothesis
        };
        let next = capsule("capsule-2", Some("failure"), Some(hypothesis)).unwrap();
        assert_eq!(
            next.verify_retry_from(&previous),
            Err(CheckpointError::UnchangedRetryStrategy)
        );
    }

    #[test]
    fn retry_rejects_authority_drift() {
        let previous = capsule("capsule-1", None, None).unwrap();
        let hypothesis = RetryHypothesis {
            failure_fingerprint: digest("failure"),
            previous_capsule_fingerprint: previous.fingerprint.clone(),
            previous_hypothesis: None,
            disproving_evidence: vec![digest("compiler-output")],
            new_hypothesis: "the fixture is stale".into(),
            changed_strategy: "update only the bounded fixture".into(),
            environment_fingerprint: digest("env"),
        };
        let mut next = capsule("capsule-2", Some("failure"), Some(hypothesis)).unwrap();
        next.allowed_paths.push("../outside-authority".into());
        next.fingerprint = next.calculate_fingerprint();
        assert_eq!(
            next.verify_retry_from(&previous),
            Err(CheckpointError::RetryAuthorityMismatch)
        );
    }

    #[test]
    fn retry_rejects_nonconsecutive_capsule_replay() {
        let root = capsule("capsule-root", None, None).unwrap();
        let first_hypothesis = RetryHypothesis {
            failure_fingerprint: digest("failure"),
            previous_capsule_fingerprint: root.fingerprint.clone(),
            previous_hypothesis: None,
            disproving_evidence: vec![digest("compiler-output")],
            new_hypothesis: "first hypothesis".into(),
            changed_strategy: "first changed strategy".into(),
            environment_fingerprint: digest("env-1"),
        };
        let first = capsule("capsule-a", Some("failure"), Some(first_hypothesis)).unwrap();
        first.verify_retry_from(&root).unwrap();
        let second_hypothesis = RetryHypothesis {
            failure_fingerprint: digest("failure"),
            previous_capsule_fingerprint: first.fingerprint.clone(),
            previous_hypothesis: Some("first hypothesis".into()),
            disproving_evidence: vec![digest("targeted-test")],
            new_hypothesis: "second hypothesis".into(),
            changed_strategy: "second changed strategy".into(),
            environment_fingerprint: digest("env-2"),
        };
        let second = capsule("capsule-b", Some("failure"), Some(second_hypothesis)).unwrap();
        second.verify_retry_from(&first).unwrap();
        assert_eq!(
            first.verify_retry_from(&second),
            Err(CheckpointError::RetryLineageMismatch)
        );
    }

    #[test]
    fn capsule_fingerprint_detects_context_tampering() {
        let mut value = capsule("capsule-1", None, None).unwrap();
        value.objective = "silently broaden the task".into();
        assert_eq!(value.verify(), Err(CheckpointError::FingerprintMismatch));
    }
}
