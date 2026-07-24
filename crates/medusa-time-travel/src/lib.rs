use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionState {
    pub execution_id: String,
    pub sequence: u64,
    pub values: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FullSnapshot {
    pub execution_id: String,
    pub sequence: u64,
    pub state: ExecutionState,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateDelta {
    pub execution_id: String,
    pub from_sequence: u64,
    pub to_sequence: u64,
    pub base_fingerprint: String,
    pub upserts: BTreeMap<String, String>,
    pub removals: BTreeSet<String>,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotIndexEntry {
    pub sequence: u64,
    pub snapshot_fingerprint: String,
    pub delta_fingerprints: Vec<String>,
}

#[derive(Debug, Default)]
pub struct TimeTravelStore {
    snapshots: BTreeMap<String, FullSnapshot>,
    deltas: BTreeMap<String, StateDelta>,
    index: BTreeMap<String, BTreeMap<u64, SnapshotIndexEntry>>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TimeTravelError {
    #[error("execution identifier mismatch")]
    ExecutionMismatch,
    #[error("state sequence must advance")]
    NonAdvancingSequence,
    #[error("snapshot fingerprint is invalid")]
    InvalidSnapshotFingerprint,
    #[error("delta fingerprint is invalid")]
    InvalidDeltaFingerprint,
    #[error("base snapshot is missing")]
    MissingBaseSnapshot,
    #[error("delta base fingerprint mismatch")]
    BaseFingerprintMismatch,
    #[error("snapshot index entry is missing")]
    MissingIndexEntry,
    #[error("requested sequence cannot be restored")]
    UnrestorableSequence,
}

impl FullSnapshot {
    pub fn new(state: ExecutionState) -> Self {
        let execution_id = state.execution_id.clone();
        let sequence = state.sequence;
        let fingerprint = digest(&SnapshotPayload {
            execution_id: &execution_id,
            sequence,
            state: &state,
        });
        Self { execution_id, sequence, state, fingerprint }
    }

    pub fn verify(&self) -> Result<(), TimeTravelError> {
        let expected = digest(&SnapshotPayload {
            execution_id: &self.execution_id,
            sequence: self.sequence,
            state: &self.state,
        });
        if expected == self.fingerprint && self.state.execution_id == self.execution_id && self.state.sequence == self.sequence {
            Ok(())
        } else {
            Err(TimeTravelError::InvalidSnapshotFingerprint)
        }
    }
}

impl StateDelta {
    pub fn between(base: &FullSnapshot, target: &ExecutionState) -> Result<Self, TimeTravelError> {
        base.verify()?;
        if base.execution_id != target.execution_id {
            return Err(TimeTravelError::ExecutionMismatch);
        }
        if target.sequence <= base.sequence {
            return Err(TimeTravelError::NonAdvancingSequence);
        }

        let upserts = target
            .values
            .iter()
            .filter(|(key, value)| base.state.values.get(*key) != Some(*value))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let removals = base
            .state
            .values
            .keys()
            .filter(|key| !target.values.contains_key(*key))
            .cloned()
            .collect();

        let mut delta = Self {
            execution_id: target.execution_id.clone(),
            from_sequence: base.sequence,
            to_sequence: target.sequence,
            base_fingerprint: base.fingerprint.clone(),
            upserts,
            removals,
            fingerprint: String::new(),
        };
        delta.fingerprint = delta.expected_fingerprint();
        Ok(delta)
    }

    pub fn verify(&self) -> Result<(), TimeTravelError> {
        if self.to_sequence <= self.from_sequence || self.expected_fingerprint() != self.fingerprint {
            return Err(TimeTravelError::InvalidDeltaFingerprint);
        }
        Ok(())
    }

    fn expected_fingerprint(&self) -> String {
        digest(&DeltaPayload {
            execution_id: &self.execution_id,
            from_sequence: self.from_sequence,
            to_sequence: self.to_sequence,
            base_fingerprint: &self.base_fingerprint,
            upserts: &self.upserts,
            removals: &self.removals,
        })
    }
}

impl TimeTravelStore {
    pub fn insert_snapshot(&mut self, snapshot: FullSnapshot) -> Result<String, TimeTravelError> {
        snapshot.verify()?;
        let fingerprint = snapshot.fingerprint.clone();
        self.index
            .entry(snapshot.execution_id.clone())
            .or_default()
            .entry(snapshot.sequence)
            .or_insert_with(|| SnapshotIndexEntry {
                sequence: snapshot.sequence,
                snapshot_fingerprint: fingerprint.clone(),
                delta_fingerprints: Vec::new(),
            });
        self.snapshots.entry(fingerprint.clone()).or_insert(snapshot);
        Ok(fingerprint)
    }

    pub fn insert_delta(&mut self, delta: StateDelta) -> Result<String, TimeTravelError> {
        delta.verify()?;
        let base = self
            .snapshots
            .get(&delta.base_fingerprint)
            .ok_or(TimeTravelError::MissingBaseSnapshot)?;
        if base.execution_id != delta.execution_id || base.sequence != delta.from_sequence {
            return Err(TimeTravelError::BaseFingerprintMismatch);
        }
        let fingerprint = delta.fingerprint.clone();
        let entries = self.index.entry(delta.execution_id.clone()).or_default();
        let base_entry = entries
            .get_mut(&delta.from_sequence)
            .ok_or(TimeTravelError::MissingIndexEntry)?;
        if !base_entry.delta_fingerprints.contains(&fingerprint) {
            base_entry.delta_fingerprints.push(fingerprint.clone());
            base_entry.delta_fingerprints.sort();
        }
        self.deltas.entry(fingerprint.clone()).or_insert(delta);
        Ok(fingerprint)
    }

    pub fn restore(&self, execution_id: &str, sequence: u64) -> Result<ExecutionState, TimeTravelError> {
        let entries = self.index.get(execution_id).ok_or(TimeTravelError::MissingIndexEntry)?;
        if let Some(entry) = entries.get(&sequence) {
            return self
                .snapshots
                .get(&entry.snapshot_fingerprint)
                .cloned()
                .ok_or(TimeTravelError::MissingBaseSnapshot)
                .and_then(|snapshot| {
                    snapshot.verify()?;
                    Ok(snapshot.state)
                });
        }

        for entry in entries.values().rev().filter(|entry| entry.sequence < sequence) {
            let snapshot = self
                .snapshots
                .get(&entry.snapshot_fingerprint)
                .ok_or(TimeTravelError::MissingBaseSnapshot)?;
            snapshot.verify()?;
            for fingerprint in &entry.delta_fingerprints {
                let delta = self.deltas.get(fingerprint).ok_or(TimeTravelError::UnrestorableSequence)?;
                delta.verify()?;
                if delta.to_sequence == sequence {
                    return apply_delta(snapshot, delta);
                }
            }
        }
        Err(TimeTravelError::UnrestorableSequence)
    }

    pub fn garbage_collect(&mut self, execution_id: &str, retain_from_sequence: u64) -> Result<usize, TimeTravelError> {
        let Some(entries) = self.index.get_mut(execution_id) else {
            return Ok(0);
        };
        let removable: Vec<u64> = entries.keys().copied().filter(|sequence| *sequence < retain_from_sequence).collect();
        let retained_snapshots: BTreeSet<String> = entries
            .iter()
            .filter(|(sequence, _)| **sequence >= retain_from_sequence)
            .map(|(_, entry)| entry.snapshot_fingerprint.clone())
            .collect();
        let retained_deltas: BTreeSet<String> = entries
            .iter()
            .filter(|(sequence, _)| **sequence >= retain_from_sequence)
            .flat_map(|(_, entry)| entry.delta_fingerprints.iter().cloned())
            .collect();
        let removed = removable.len();
        for sequence in removable {
            entries.remove(&sequence);
        }
        self.snapshots.retain(|fingerprint, _| retained_snapshots.contains(fingerprint));
        self.deltas.retain(|fingerprint, _| retained_deltas.contains(fingerprint));
        Ok(removed)
    }
}

fn apply_delta(base: &FullSnapshot, delta: &StateDelta) -> Result<ExecutionState, TimeTravelError> {
    if base.fingerprint != delta.base_fingerprint || base.execution_id != delta.execution_id {
        return Err(TimeTravelError::BaseFingerprintMismatch);
    }
    let mut values = base.state.values.clone();
    for key in &delta.removals {
        values.remove(key);
    }
    values.extend(delta.upserts.clone());
    Ok(ExecutionState { execution_id: delta.execution_id.clone(), sequence: delta.to_sequence, values })
}

#[derive(Serialize)]
struct SnapshotPayload<'a> {
    execution_id: &'a str,
    sequence: u64,
    state: &'a ExecutionState,
}

#[derive(Serialize)]
struct DeltaPayload<'a> {
    execution_id: &'a str,
    from_sequence: u64,
    to_sequence: u64,
    base_fingerprint: &'a str,
    upserts: &'a BTreeMap<String, String>,
    removals: &'a BTreeSet<String>,
}

fn digest<T: Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).expect("serializable snapshot payload");
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(sequence: u64, pairs: &[(&str, &str)]) -> ExecutionState {
        ExecutionState {
            execution_id: "exec-1".into(),
            sequence,
            values: pairs.iter().map(|(key, value)| ((*key).into(), (*value).into())).collect(),
        }
    }

    #[test]
    fn restores_exact_snapshot_and_delta_point() {
        let base = FullSnapshot::new(state(10, &[("phase", "leased"), ("worker", "a")]));
        let target = state(14, &[("phase", "prepared"), ("barrier", "ready")]);
        let delta = StateDelta::between(&base, &target).unwrap();
        let mut store = TimeTravelStore::default();
        store.insert_snapshot(base).unwrap();
        store.insert_delta(delta).unwrap();

        assert_eq!(store.restore("exec-1", 14).unwrap(), target);
    }

    #[test]
    fn content_addressing_deduplicates_snapshots() {
        let snapshot = FullSnapshot::new(state(1, &[("phase", "created")]));
        let mut store = TimeTravelStore::default();
        let first = store.insert_snapshot(snapshot.clone()).unwrap();
        let second = store.insert_snapshot(snapshot).unwrap();
        assert_eq!(first, second);
        assert_eq!(store.snapshots.len(), 1);
    }

    #[test]
    fn detects_snapshot_tampering() {
        let mut snapshot = FullSnapshot::new(state(1, &[("phase", "created")]));
        snapshot.state.values.insert("phase".into(), "failed".into());
        assert_eq!(snapshot.verify(), Err(TimeTravelError::InvalidSnapshotFingerprint));
    }

    #[test]
    fn detects_delta_tampering() {
        let base = FullSnapshot::new(state(2, &[("phase", "scheduled")]));
        let mut delta = StateDelta::between(&base, &state(3, &[("phase", "leased")])).unwrap();
        delta.upserts.insert("worker".into(), "unexpected".into());
        assert_eq!(delta.verify(), Err(TimeTravelError::InvalidDeltaFingerprint));
    }

    #[test]
    fn rejects_cross_execution_deltas() {
        let base = FullSnapshot::new(state(1, &[]));
        let mut target = state(2, &[]);
        target.execution_id = "exec-2".into();
        assert_eq!(StateDelta::between(&base, &target), Err(TimeTravelError::ExecutionMismatch));
    }

    #[test]
    fn garbage_collection_preserves_retained_boundary() {
        let mut store = TimeTravelStore::default();
        store.insert_snapshot(FullSnapshot::new(state(1, &[("phase", "created")]))).unwrap();
        store.insert_snapshot(FullSnapshot::new(state(5, &[("phase", "executing")]))).unwrap();
        assert_eq!(store.garbage_collect("exec-1", 5).unwrap(), 1);
        assert!(store.restore("exec-1", 1).is_err());
        assert_eq!(store.restore("exec-1", 5).unwrap().sequence, 5);
    }
}
