//! Structured runtime liveness reports consumed by the daemon control plane.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeShutdownState {
    Running,
    Clean,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeSupervisionReport {
    pub process_id: String,
    pub execution_id: String,
    pub installation_id: String,
    pub generation: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub observed_at: OffsetDateTime,
    pub checkpoint_fingerprint: Option<String>,
    pub shutdown: RuntimeShutdownState,
}

impl RuntimeSupervisionReport {
    pub fn heartbeat(
        process_id: impl Into<String>,
        execution_id: impl Into<String>,
        installation_id: impl Into<String>,
        generation: u64,
        observed_at: OffsetDateTime,
        checkpoint_fingerprint: Option<String>,
    ) -> Result<Self, &'static str> {
        let report = Self {
            process_id: process_id.into(),
            execution_id: execution_id.into(),
            installation_id: installation_id.into(),
            generation,
            observed_at,
            checkpoint_fingerprint,
            shutdown: RuntimeShutdownState::Running,
        };
        report.validate()?;
        Ok(report)
    }

    pub fn shutdown(
        mut self,
        clean: bool,
        observed_at: OffsetDateTime,
    ) -> Result<Self, &'static str> {
        if observed_at < self.observed_at {
            return Err("runtime supervision timestamp regressed");
        }
        self.observed_at = observed_at;
        self.shutdown = if clean {
            RuntimeShutdownState::Clean
        } else {
            RuntimeShutdownState::Failed
        };
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.process_id.trim().is_empty()
            || self.execution_id.trim().is_empty()
            || self.installation_id.trim().is_empty()
            || self.generation == 0
        {
            return Err("runtime supervision identity is invalid");
        }
        if let Some(fingerprint) = self.checkpoint_fingerprint.as_deref() {
            if fingerprint.len() != 64 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err("checkpoint fingerprint must be a SHA-256 digest");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn heartbeat_contains_stable_runtime_identity_and_checkpoint() {
        let report = RuntimeSupervisionReport::heartbeat(
            "runtime-1",
            "exec-1",
            "install-1",
            2,
            datetime!(2026-07-26 08:00 UTC),
            Some("ab".repeat(32)),
        )
        .unwrap();
        assert_eq!(report.shutdown, RuntimeShutdownState::Running);
        report.validate().unwrap();
    }

    #[test]
    fn shutdown_is_explicit_and_timestamp_ordered() {
        let report = RuntimeSupervisionReport::heartbeat(
            "runtime-1",
            "exec-1",
            "install-1",
            1,
            datetime!(2026-07-26 08:00 UTC),
            None,
        )
        .unwrap()
        .shutdown(true, datetime!(2026-07-26 08:01 UTC))
        .unwrap();
        assert_eq!(report.shutdown, RuntimeShutdownState::Clean);
    }
}
