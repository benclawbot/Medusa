#[path = "lib.rs"]
mod original;

pub use original::*;
pub mod frontend;

pub mod supervision {
    use serde::{Deserialize, Serialize};
    use time::OffsetDateTime;

    #[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum RecoveryDisposition {
        Resume,
        Retry,
        RollBack,
        NoOp,
        Terminal,
    }

    #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum SupervisionPayload {
        Heartbeat {
            execution_id: String,
            process_id: String,
            pid: u32,
            checkpoint_ref: Option<String>,
            #[serde(with = "time::serde::rfc3339")]
            observed_at: OffsetDateTime,
        },
        RecoveryDecision {
            execution_id: String,
            process_id: String,
            disposition: RecoveryDisposition,
            reason: String,
            evidence_fingerprint: String,
        },
        TerminalFailure {
            execution_id: String,
            process_id: String,
            reason: String,
        },
        ShutdownState {
            execution_id: String,
            process_id: String,
            requested: bool,
        },
    }

    impl SupervisionPayload {
        pub fn validate(&self) -> Result<(), &'static str> {
            match self {
                Self::Heartbeat {
                    execution_id,
                    process_id,
                    pid,
                    ..
                } => {
                    validate_ids(execution_id, process_id)?;
                    if *pid == 0 {
                        return Err("heartbeat pid cannot be zero");
                    }
                }
                Self::RecoveryDecision {
                    execution_id,
                    process_id,
                    reason,
                    evidence_fingerprint,
                    ..
                } => {
                    validate_ids(execution_id, process_id)?;
                    if reason.trim().is_empty() {
                        return Err("recovery reason cannot be empty");
                    }
                    if evidence_fingerprint.len() != 64
                        || !evidence_fingerprint
                            .bytes()
                            .all(|byte| byte.is_ascii_hexdigit())
                    {
                        return Err("recovery evidence must be a sha-256 digest");
                    }
                }
                Self::TerminalFailure {
                    execution_id,
                    process_id,
                    reason,
                } => {
                    validate_ids(execution_id, process_id)?;
                    if reason.trim().is_empty() {
                        return Err("terminal failure reason cannot be empty");
                    }
                }
                Self::ShutdownState {
                    execution_id,
                    process_id,
                    ..
                } => validate_ids(execution_id, process_id)?,
            }
            Ok(())
        }
    }

    fn validate_ids(execution_id: &str, process_id: &str) -> Result<(), &'static str> {
        if execution_id.trim().is_empty() {
            return Err("execution id cannot be empty");
        }
        if process_id.trim().is_empty() {
            return Err("process id cannot be empty");
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use time::macros::datetime;

        #[test]
        fn heartbeat_contract_round_trips() {
            let payload = SupervisionPayload::Heartbeat {
                execution_id: "exec-1".to_owned(),
                process_id: "runtime-1".to_owned(),
                pid: 42,
                checkpoint_ref: Some("checkpoint-1".to_owned()),
                observed_at: datetime!(2026-07-26 07:00 UTC),
            };
            payload.validate().expect("valid payload");
            let json = serde_json::to_string(&payload).expect("serialize");
            assert_eq!(
                serde_json::from_str::<SupervisionPayload>(&json).expect("deserialize"),
                payload
            );
        }

        #[test]
        fn invalid_recovery_evidence_is_rejected() {
            let payload = SupervisionPayload::RecoveryDecision {
                execution_id: "exec-1".to_owned(),
                process_id: "runtime-1".to_owned(),
                disposition: RecoveryDisposition::Resume,
                reason: "resume durable operation".to_owned(),
                evidence_fingerprint: "bad".to_owned(),
            };
            assert!(payload.validate().is_err());
        }
    }
}
