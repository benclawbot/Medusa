//! Deterministic verification of recorded and replayed Medusa executions.

use std::collections::{BTreeMap, BTreeSet};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionTrace {
    pub execution_id: String,
    pub snapshot: String,
    pub schedule: String,
    pub leases: String,
    pub barrier: String,
    pub rollback_journal: Option<String>,
    pub task_outputs: BTreeMap<String, String>,
    pub final_snapshot: String,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize)]
pub enum DivergenceKind {
    InitialSnapshot,
    Schedule,
    LeaseState,
    CommitDecision,
    RollbackState,
    MissingTask,
    UnexpectedTask,
    TaskOutput,
    FinalSnapshot,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Divergence {
    pub kind: DivergenceKind,
    pub subject: String,
    pub expected: Option<String>,
    pub actual: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplayReport {
    pub execution_id: String,
    pub equivalent: bool,
    pub divergences: Vec<Divergence>,
    pub expected_fingerprint: String,
    pub actual_fingerprint: String,
    pub fingerprint: String,
}

impl ExecutionTrace {
    pub fn new(
        execution_id: impl Into<String>, snapshot: impl Into<String>, schedule: impl Into<String>,
        leases: impl Into<String>, barrier: impl Into<String>, rollback_journal: Option<String>,
        task_outputs: BTreeMap<String, String>, final_snapshot: impl Into<String>,
    ) -> Result<Self, &'static str> {
        let mut trace = Self {
            execution_id: execution_id.into(), snapshot: snapshot.into(), schedule: schedule.into(),
            leases: leases.into(), barrier: barrier.into(), rollback_journal, task_outputs,
            final_snapshot: final_snapshot.into(), fingerprint: String::new(),
        };
        trace.validate_fields()?;
        trace.fingerprint = trace.calculated_fingerprint();
        Ok(trace)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        self.validate_fields()?;
        if self.fingerprint != self.calculated_fingerprint() { return Err("execution trace fingerprint mismatch"); }
        Ok(())
    }

    fn validate_fields(&self) -> Result<(), &'static str> {
        if self.execution_id.trim().is_empty() { return Err("execution identifier cannot be empty"); }
        for digest in [&self.snapshot, &self.schedule, &self.leases, &self.barrier, &self.final_snapshot] {
            validate_digest(digest)?;
        }
        if let Some(journal) = &self.rollback_journal { validate_digest(journal)?; }
        if self.task_outputs.keys().any(|id| id.trim().is_empty()) { return Err("task identifiers cannot be empty"); }
        for digest in self.task_outputs.values() { validate_digest(digest)?; }
        Ok(())
    }

    fn calculated_fingerprint(&self) -> String {
        hash(&(
            &self.execution_id, &self.snapshot, &self.schedule, &self.leases, &self.barrier,
            &self.rollback_journal, &self.task_outputs, &self.final_snapshot,
        ))
    }
}

pub fn verify(expected: &ExecutionTrace, actual: &ExecutionTrace) -> Result<ReplayReport, &'static str> {
    expected.validate()?;
    actual.validate()?;
    if expected.execution_id != actual.execution_id { return Err("execution identifiers differ"); }

    let mut divergences = Vec::new();
    compare_field(&mut divergences, DivergenceKind::InitialSnapshot, "repository", &expected.snapshot, &actual.snapshot);
    compare_field(&mut divergences, DivergenceKind::Schedule, "scheduler", &expected.schedule, &actual.schedule);
    compare_field(&mut divergences, DivergenceKind::LeaseState, "leases", &expected.leases, &actual.leases);
    compare_field(&mut divergences, DivergenceKind::CommitDecision, "barrier", &expected.barrier, &actual.barrier);
    compare_optional(&mut divergences, DivergenceKind::RollbackState, "rollback", expected.rollback_journal.as_ref(), actual.rollback_journal.as_ref());

    let ids = expected.task_outputs.keys().chain(actual.task_outputs.keys()).cloned().collect::<BTreeSet<_>>();
    for id in ids {
        match (expected.task_outputs.get(&id), actual.task_outputs.get(&id)) {
            (Some(left), Some(right)) if left != right => divergences.push(Divergence { kind: DivergenceKind::TaskOutput, subject: id, expected: Some(left.clone()), actual: Some(right.clone()) }),
            (Some(left), None) => divergences.push(Divergence { kind: DivergenceKind::MissingTask, subject: id, expected: Some(left.clone()), actual: None }),
            (None, Some(right)) => divergences.push(Divergence { kind: DivergenceKind::UnexpectedTask, subject: id, expected: None, actual: Some(right.clone()) }),
            _ => {}
        }
    }
    compare_field(&mut divergences, DivergenceKind::FinalSnapshot, "repository", &expected.final_snapshot, &actual.final_snapshot);
    divergences.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.subject.cmp(&b.subject)));

    let mut report = ReplayReport {
        execution_id: expected.execution_id.clone(), equivalent: divergences.is_empty(), divergences,
        expected_fingerprint: expected.fingerprint.clone(), actual_fingerprint: actual.fingerprint.clone(), fingerprint: String::new(),
    };
    report.fingerprint = hash(&(&report.execution_id, report.equivalent, &report.divergences, &report.expected_fingerprint, &report.actual_fingerprint));
    Ok(report)
}

impl ReplayReport {
    pub fn validate(&self) -> Result<(), &'static str> {
        let expected = hash(&(&self.execution_id, self.equivalent, &self.divergences, &self.expected_fingerprint, &self.actual_fingerprint));
        if self.fingerprint != expected { return Err("replay report fingerprint mismatch"); }
        if self.equivalent != self.divergences.is_empty() { return Err("replay equivalence is inconsistent"); }
        Ok(())
    }
}

fn compare_field(out: &mut Vec<Divergence>, kind: DivergenceKind, subject: &str, expected: &str, actual: &str) {
    if expected != actual { out.push(Divergence { kind, subject: subject.into(), expected: Some(expected.into()), actual: Some(actual.into()) }); }
}
fn compare_optional(out: &mut Vec<Divergence>, kind: DivergenceKind, subject: &str, expected: Option<&String>, actual: Option<&String>) {
    if expected != actual { out.push(Divergence { kind, subject: subject.into(), expected: expected.cloned(), actual: actual.cloned() }); }
}
fn validate_digest(value: &str) -> Result<(), &'static str> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()) { return Err("fingerprints must be lowercase SHA-256 hex"); }
    Ok(())
}
fn hash<T: Serialize>(value: &T) -> String { hex::encode(Sha256::digest(serde_json::to_vec(value).expect("replay state serializes"))) }

#[cfg(test)]
mod tests {
    use super::*;
    fn digest(ch: char) -> String { std::iter::repeat_n(ch, 64).collect() }
    fn trace() -> ExecutionTrace {
        ExecutionTrace::new("run-1", digest('a'), digest('b'), digest('c'), digest('d'), None,
            BTreeMap::from([("task-a".into(), digest('e'))]), digest('f')).unwrap()
    }

    #[test]
    fn identical_replay_is_equivalent() {
        let expected = trace();
        let report = verify(&expected, &expected).unwrap();
        assert!(report.equivalent);
        assert!(report.divergences.is_empty());
        report.validate().unwrap();
    }

    #[test]
    fn reports_canonical_task_and_snapshot_divergence() {
        let expected = trace();
        let mut actual = trace();
        actual.task_outputs.insert("task-b".into(), digest('1'));
        actual.final_snapshot = digest('2');
        actual.fingerprint = actual.calculated_fingerprint();
        let report = verify(&expected, &actual).unwrap();
        assert_eq!(report.divergences[0].kind, DivergenceKind::UnexpectedTask);
        assert_eq!(report.divergences[1].kind, DivergenceKind::FinalSnapshot);
    }

    #[test]
    fn detects_tampered_trace_and_report() {
        let expected = trace();
        let mut tampered = expected.clone();
        tampered.schedule = digest('1');
        assert_eq!(verify(&expected, &tampered), Err("execution trace fingerprint mismatch"));
        let mut report = verify(&expected, &expected).unwrap();
        report.equivalent = false;
        assert!(report.validate().is_err());
    }
}
