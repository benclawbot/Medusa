//! Durable orchestration state for Medusa's distributed execution runtime.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Phase {
    Created,
    Scheduled,
    Leased,
    Executing,
    Prepared,
    Committing,
    Verifying,
    Completed,
    Recovering,
    RolledBack,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Signal {
    ScheduleReady { fingerprint: String },
    LeasesReady { fingerprint: String },
    WorkersStarted,
    BarrierPrepared { fingerprint: String },
    CommitStarted { journal_fingerprint: String },
    CommitApplied { final_snapshot: String },
    ReplayVerified { report_fingerprint: String },
    RecoveryRequired { reason: String },
    RollbackComplete { snapshot: String },
    TerminalFailure { reason: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SupervisorState {
    pub execution_id: String,
    pub phase: Phase,
    pub schedule_fingerprint: Option<String>,
    pub lease_fingerprint: Option<String>,
    pub barrier_fingerprint: Option<String>,
    pub journal_fingerprint: Option<String>,
    pub final_snapshot: Option<String>,
    pub replay_report_fingerprint: Option<String>,
    pub failure_reason: Option<String>,
    pub sequence: u64,
    pub fingerprint: String,
}

impl SupervisorState {
    pub fn new(execution_id: impl Into<String>) -> Result<Self, &'static str> {
        let execution_id = execution_id.into();
        if execution_id.trim().is_empty() { return Err("execution identifier cannot be empty"); }
        let mut state = Self {
            execution_id,
            phase: Phase::Created,
            schedule_fingerprint: None,
            lease_fingerprint: None,
            barrier_fingerprint: None,
            journal_fingerprint: None,
            final_snapshot: None,
            replay_report_fingerprint: None,
            failure_reason: None,
            sequence: 0,
            fingerprint: String::new(),
        };
        state.seal();
        Ok(state)
    }

    pub fn apply(&mut self, signal: Signal) -> Result<(), &'static str> {
        self.validate()?;
        match (&self.phase, signal) {
            (Phase::Created, Signal::ScheduleReady { fingerprint }) => {
                require_digest(&fingerprint)?;
                self.schedule_fingerprint = Some(fingerprint);
                self.phase = Phase::Scheduled;
            }
            (Phase::Scheduled, Signal::LeasesReady { fingerprint }) => {
                require_digest(&fingerprint)?;
                self.lease_fingerprint = Some(fingerprint);
                self.phase = Phase::Leased;
            }
            (Phase::Leased, Signal::WorkersStarted) => self.phase = Phase::Executing,
            (Phase::Executing, Signal::BarrierPrepared { fingerprint }) => {
                require_digest(&fingerprint)?;
                self.barrier_fingerprint = Some(fingerprint);
                self.phase = Phase::Prepared;
            }
            (Phase::Prepared, Signal::CommitStarted { journal_fingerprint }) => {
                require_digest(&journal_fingerprint)?;
                self.journal_fingerprint = Some(journal_fingerprint);
                self.phase = Phase::Committing;
            }
            (Phase::Committing, Signal::CommitApplied { final_snapshot }) => {
                require_digest(&final_snapshot)?;
                self.final_snapshot = Some(final_snapshot);
                self.phase = Phase::Verifying;
            }
            (Phase::Verifying, Signal::ReplayVerified { report_fingerprint }) => {
                require_digest(&report_fingerprint)?;
                self.replay_report_fingerprint = Some(report_fingerprint);
                self.phase = Phase::Completed;
            }
            (Phase::Created | Phase::Scheduled | Phase::Leased | Phase::Executing | Phase::Prepared | Phase::Committing | Phase::Verifying,
             Signal::RecoveryRequired { reason }) => {
                require_reason(&reason)?;
                self.failure_reason = Some(reason);
                self.phase = Phase::Recovering;
            }
            (Phase::Recovering, Signal::RollbackComplete { snapshot }) => {
                require_digest(&snapshot)?;
                self.final_snapshot = Some(snapshot);
                self.phase = Phase::RolledBack;
            }
            (Phase::Created | Phase::Scheduled | Phase::Leased | Phase::Executing | Phase::Prepared | Phase::Committing | Phase::Verifying | Phase::Recovering,
             Signal::TerminalFailure { reason }) => {
                require_reason(&reason)?;
                self.failure_reason = Some(reason);
                self.phase = Phase::Failed;
            }
            _ => return Err("signal is invalid for the current supervisor phase"),
        }
        self.sequence = self.sequence.checked_add(1).ok_or("supervisor sequence overflow")?;
        self.seal();
        Ok(())
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.execution_id.trim().is_empty() { return Err("execution identifier cannot be empty"); }
        for digest in [
            self.schedule_fingerprint.as_ref(), self.lease_fingerprint.as_ref(),
            self.barrier_fingerprint.as_ref(), self.journal_fingerprint.as_ref(),
            self.final_snapshot.as_ref(), self.replay_report_fingerprint.as_ref(),
        ].into_iter().flatten() { require_digest(digest)?; }
        if self.fingerprint != self.calculate_fingerprint() { return Err("supervisor fingerprint does not match state"); }
        Ok(())
    }

    pub fn resumable(&self) -> bool {
        matches!(self.phase, Phase::Created | Phase::Scheduled | Phase::Leased | Phase::Executing | Phase::Prepared | Phase::Committing | Phase::Verifying | Phase::Recovering)
    }

    fn calculate_fingerprint(&self) -> String {
        let mut copy = self.clone();
        copy.fingerprint.clear();
        hex::encode(Sha256::digest(serde_json::to_vec(&copy).expect("supervisor state serializes")))
    }

    fn seal(&mut self) { self.fingerprint = self.calculate_fingerprint(); }
}

fn require_digest(value: &str) -> Result<(), &'static str> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("fingerprint must be a SHA-256 hexadecimal digest");
    }
    Ok(())
}

fn require_reason(value: &str) -> Result<(), &'static str> {
    if value.trim().is_empty() { return Err("failure reason cannot be empty"); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn digest(ch: char) -> String { std::iter::repeat_n(ch, 64).collect() }

    #[test]
    fn completes_the_full_execution_lifecycle() {
        let mut state = SupervisorState::new("exec-1").unwrap();
        state.apply(Signal::ScheduleReady { fingerprint: digest('a') }).unwrap();
        state.apply(Signal::LeasesReady { fingerprint: digest('b') }).unwrap();
        state.apply(Signal::WorkersStarted).unwrap();
        state.apply(Signal::BarrierPrepared { fingerprint: digest('c') }).unwrap();
        state.apply(Signal::CommitStarted { journal_fingerprint: digest('d') }).unwrap();
        state.apply(Signal::CommitApplied { final_snapshot: digest('e') }).unwrap();
        state.apply(Signal::ReplayVerified { report_fingerprint: digest('f') }).unwrap();
        assert_eq!(state.phase, Phase::Completed);
        assert!(!state.resumable());
        state.validate().unwrap();
    }

    #[test]
    fn interruption_transitions_to_recovery_and_rollback() {
        let mut state = SupervisorState::new("exec-2").unwrap();
        state.apply(Signal::ScheduleReady { fingerprint: digest('a') }).unwrap();
        state.apply(Signal::RecoveryRequired { reason: "worker lease expired".into() }).unwrap();
        assert_eq!(state.phase, Phase::Recovering);
        state.apply(Signal::RollbackComplete { snapshot: digest('b') }).unwrap();
        assert_eq!(state.phase, Phase::RolledBack);
    }

    #[test]
    fn rejects_out_of_order_and_tampered_state() {
        let mut state = SupervisorState::new("exec-3").unwrap();
        assert!(state.apply(Signal::WorkersStarted).is_err());
        state.sequence = 99;
        assert!(state.validate().is_err());
    }
}
