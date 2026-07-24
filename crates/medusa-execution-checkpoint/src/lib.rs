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
        Ok(Self { execution_id, events: Vec::new(), checkpoints: Vec::new() })
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
        Ok(self.events.last().expect("event was just appended"))
    }

    pub fn add_checkpoint(&mut self, checkpoint: ExecutionCheckpoint) -> Result<(), CheckpointError> {
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
            Some(self.events[(checkpoint.sequence - 1) as usize].fingerprint.clone())
        };
        if checkpoint.last_event_fingerprint != expected_last {
            return Err(CheckpointError::CheckpointEventMismatch);
        }
        if self.checkpoints.last().is_some_and(|last| checkpoint.sequence <= last.sequence) {
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
        let start = self.latest_checkpoint().map_or(0, |checkpoint| checkpoint.sequence as usize);
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
                Some(self.events[(checkpoint.sequence - 1) as usize].fingerprint.as_str())
            };
            if checkpoint.last_event_fingerprint.as_deref() != expected {
                return Err(CheckpointError::CheckpointEventMismatch);
            }
            last_sequence = Some(checkpoint.sequence);
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
}

fn validate_sha256(value: &str) -> Result<(), CheckpointError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) {
        return Err(CheckpointError::InvalidFingerprint);
    }
    Ok(())
}

fn hash_json<T: Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).expect("serializing canonical checkpoint data cannot fail");
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
            if sequence == 0 { None } else { Some(log.events[(sequence - 1) as usize].fingerprint.clone()) },
            BTreeMap::from([("leases".into(), digest("leases"))]),
        ).unwrap()
    }

    #[test]
    fn appends_hash_chained_events_and_verifies() {
        let mut log = ExecutionLog::new("run-1").unwrap();
        log.append_event("scheduled", digest("schedule")).unwrap();
        log.append_event("leased", digest("leases")).unwrap();
        assert_eq!(log.events[1].previous_event_fingerprint.as_deref(), Some(log.events[0].fingerprint.as_str()));
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
        assert_eq!(log.add_checkpoint(value), Err(CheckpointError::CheckpointEventMismatch));
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
        assert_eq!(log.add_checkpoint(checkpoint(&log, 1)), Err(CheckpointError::NonMonotonicCheckpoint));
    }
}
