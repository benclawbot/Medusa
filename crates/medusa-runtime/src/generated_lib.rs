include!("lib.rs");

pub mod supervision {
    use std::{fs, path::PathBuf};

    use serde::{Deserialize, Serialize};
    use thiserror::Error;
    use time::OffsetDateTime;

    #[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum RuntimeHeartbeatState {
        Starting,
        Running,
        ShuttingDown,
        Stopped,
        Failed,
    }

    #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
    pub struct RuntimeHeartbeat {
        pub execution_id: String,
        pub process_id: String,
        pub pid: u32,
        pub checkpoint_ref: Option<String>,
        pub state: RuntimeHeartbeatState,
        #[serde(with = "time::serde::rfc3339")]
        pub observed_at: OffsetDateTime,
    }

    pub struct RuntimeHeartbeatPublisher {
        path: PathBuf,
        execution_id: String,
        process_id: String,
        pid: u32,
    }

    impl RuntimeHeartbeatPublisher {
        pub fn new(
            path: impl Into<PathBuf>,
            execution_id: impl Into<String>,
            process_id: impl Into<String>,
            pid: u32,
        ) -> Result<Self, HeartbeatError> {
            let execution_id = execution_id.into();
            let process_id = process_id.into();
            if execution_id.trim().is_empty() {
                return Err(HeartbeatError::InvalidExecutionId);
            }
            if process_id.trim().is_empty() {
                return Err(HeartbeatError::InvalidProcessId);
            }
            if pid == 0 {
                return Err(HeartbeatError::InvalidPid);
            }
            Ok(Self {
                path: path.into(),
                execution_id,
                process_id,
                pid,
            })
        }

        pub fn publish(
            &self,
            state: RuntimeHeartbeatState,
            checkpoint_ref: Option<String>,
            observed_at: OffsetDateTime,
        ) -> Result<RuntimeHeartbeat, HeartbeatError> {
            let heartbeat = RuntimeHeartbeat {
                execution_id: self.execution_id.clone(),
                process_id: self.process_id.clone(),
                pid: self.pid,
                checkpoint_ref,
                state,
                observed_at,
            };
            let parent = self
                .path
                .parent()
                .ok_or(HeartbeatError::MissingParentDirectory)?;
            fs::create_dir_all(parent)?;
            let temporary = self.path.with_extension("json.tmp");
            fs::write(&temporary, serde_json::to_vec_pretty(&heartbeat)?)?;
            fs::rename(temporary, &self.path)?;
            Ok(heartbeat)
        }

        pub fn load(path: impl Into<PathBuf>) -> Result<RuntimeHeartbeat, HeartbeatError> {
            let bytes = fs::read(path.into())?;
            let heartbeat: RuntimeHeartbeat = serde_json::from_slice(&bytes)?;
            validate_heartbeat(&heartbeat)?;
            Ok(heartbeat)
        }
    }

    fn validate_heartbeat(heartbeat: &RuntimeHeartbeat) -> Result<(), HeartbeatError> {
        if heartbeat.execution_id.trim().is_empty() {
            return Err(HeartbeatError::InvalidExecutionId);
        }
        if heartbeat.process_id.trim().is_empty() {
            return Err(HeartbeatError::InvalidProcessId);
        }
        if heartbeat.pid == 0 {
            return Err(HeartbeatError::InvalidPid);
        }
        Ok(())
    }

    #[derive(Debug, Error)]
    pub enum HeartbeatError {
        #[error("execution id cannot be empty")]
        InvalidExecutionId,
        #[error("process id cannot be empty")]
        InvalidProcessId,
        #[error("process id 0 is invalid")]
        InvalidPid,
        #[error("heartbeat path has no parent directory")]
        MissingParentDirectory,
        #[error(transparent)]
        Io(#[from] std::io::Error),
        #[error(transparent)]
        Json(#[from] serde_json::Error),
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use tempfile::tempdir;
        use time::macros::datetime;

        #[test]
        fn heartbeat_round_trips_atomically() {
            let directory = tempdir().expect("tempdir");
            let path = directory.path().join("runtime.json");
            let publisher = RuntimeHeartbeatPublisher::new(&path, "exec-1", "runtime-1", 42)
                .expect("publisher");
            let written = publisher
                .publish(
                    RuntimeHeartbeatState::Running,
                    Some("checkpoint-1".to_owned()),
                    datetime!(2026-07-26 07:00 UTC),
                )
                .expect("publish");
            assert_eq!(
                RuntimeHeartbeatPublisher::load(&path).expect("load"),
                written
            );
        }

        #[test]
        fn zero_pid_is_rejected() {
            assert!(
                RuntimeHeartbeatPublisher::new("heartbeat.json", "exec-1", "runtime-1", 0).is_err()
            );
        }
    }
}
