use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Version of the daemon wire protocol.
pub const DAEMON_PROTOCOL_VERSION: u16 = 1;

/// Durable job lifecycle.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Interrupted,
}

/// One durable daemon-owned process record.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JobRecord {
    pub id: String,
    pub program: String,
    pub args: Vec<String>,
    pub state: JobState,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Client request envelope.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RequestEnvelope {
    pub version: u16,
    pub request: Request,
}

/// Supported daemon requests.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Ping,
    Submit { program: String, args: Vec<String> },
    Status { job_id: String },
    List,
    Shutdown,
}

/// Server response envelope.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResponseEnvelope {
    pub version: u16,
    pub response: Response,
}

/// Supported daemon responses.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Pong,
    Submitted { job: JobRecord },
    Status { job: Option<JobRecord> },
    Jobs { jobs: Vec<JobRecord> },
    Ack,
    Error { code: String, message: String },
}
