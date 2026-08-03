#!/usr/bin/env python3
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

def read(path: str) -> str:
    return (ROOT / path).read_text()

def write(path: str, content: str) -> None:
    target = ROOT / path
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(content)

def replace_once(path: str, old: str, new: str) -> None:
    content = read(path)
    count = content.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one anchor, found {count}: {old[:120]!r}")
    write(path, content.replace(old, new, 1))

replace_once(
    "crates/medusa-protocol/src/frontend/command.rs",
    '''    CreateSession {
        repository_profile: String,
        objective: Option<String>,
    },
''',
    '''    CreateSession {
        repository_profile: String,
        objective: Option<String>,
        #[serde(default)]
        attachment_ids: Vec<String>,
    },
''',
)
replace_once(
    "crates/medusa-protocol/src/frontend/command.rs",
    '''    Detach,
    Submit {
''',
    '''    Detach,
    Replay {
        after_cursor: u64,
    },
    PollTransient,
    NewSession,
    RunCommand {
        input: String,
    },
    RecoveryAction {
        operation: String,
        checkpoint_id: Option<String>,
        confirmed_destructive_effects: bool,
    },
    Submit {
''',
)
replace_once(
    "crates/medusa-protocol/src/frontend/command.rs",
    '''            Self::CreateSession {
                repository_profile, ..
            } if repository_profile.trim().is_empty() => Err("repository profile cannot be empty"),
''',
    '''            Self::CreateSession {
                repository_profile, ..
            } if repository_profile.trim().is_empty() => Err("repository profile cannot be empty"),
            Self::CreateSession {
                objective,
                attachment_ids,
                ..
            } if objective
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
                && attachment_ids.is_empty() =>
            {
                Err("session creation must contain text or an attachment")
            }
''',
)
replace_once(
    "crates/medusa-protocol/src/frontend/command.rs",
    '''            Self::ResolveApproval { approval_id, .. } if approval_id.trim().is_empty() => {
                Err("approval id cannot be empty")
            }
''',
    '''            Self::ResolveApproval { approval_id, .. } if approval_id.trim().is_empty() => {
                Err("approval id cannot be empty")
            }
            Self::RunCommand { input }
                if input.trim().is_empty() || !input.trim_start().starts_with('/') =>
            {
                Err("runtime command must be a slash command")
            }
            Self::RecoveryAction { operation, .. } if operation.trim().is_empty() => {
                Err("recovery operation cannot be empty")
            }
''',
)

write("crates/medusa-daemon/src/protocol.rs", r'''use medusa_protocol::frontend::FrontendCommandEnvelope;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::frontend_control::FrontendCommandAcknowledgement;

pub const DAEMON_PROTOCOL_VERSION: u16 = 2;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Interrupted,
}

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

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendArtifactKind {
    File,
    Image,
    #[default]
    Text,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FrontendArtifactUpload {
    pub display_name: String,
    pub mime_type: Option<String>,
    pub kind: FrontendArtifactKind,
    pub bytes_base64: String,
}

impl std::fmt::Debug for FrontendArtifactUpload {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FrontendArtifactUpload")
            .field("display_name", &self.display_name)
            .field("mime_type", &self.mime_type)
            .field("kind", &self.kind)
            .field(
                "bytes_base64",
                &format_args!("<{} encoded bytes>", self.bytes_base64.len()),
            )
            .finish()
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FrontendCredentialUpdate {
    pub provider: String,
    pub credential: String,
}

impl std::fmt::Debug for FrontendCredentialUpdate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FrontendCredentialUpdate")
            .field("provider", &self.provider)
            .field("credential", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RequestEnvelope {
    pub version: u16,
    pub request: Request,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Ping,
    Submit { program: String, args: Vec<String> },
    Status { job_id: String },
    Cancel { job_id: String },
    List,
    Frontend { envelope: FrontendCommandEnvelope },
    FrontendArtifact { upload: FrontendArtifactUpload },
    FrontendCredential { update: FrontendCredentialUpdate },
    Shutdown,
    ShutdownNow,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ResponseEnvelope {
    pub version: u16,
    pub response: Response,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Pong,
    Submitted { job: JobRecord },
    Status { job: Option<JobRecord> },
    Cancelled { job: Option<JobRecord> },
    Jobs { jobs: Vec<JobRecord> },
    Frontend {
        acknowledgement: FrontendCommandAcknowledgement,
    },
    FrontendArtifact {
        artifact_id: String,
    },
    Ack,
    Error {
        code: String,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_debug_never_exposes_the_secret() {
        let update = FrontendCredentialUpdate {
            provider: "minimax".to_owned(),
            credential: "top-secret".to_owned(),
        };
        let debug = format!("{update:?}");
        assert!(!debug.contains("top-secret"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn artifact_debug_never_exposes_payload_bytes() {
        let upload = FrontendArtifactUpload {
            display_name: "context.txt".to_owned(),
            mime_type: Some("text/plain".to_owned()),
            kind: FrontendArtifactKind::Text,
            bytes_base64: "dG9wLXNlY3JldA==".to_owned(),
        };
        let debug = format!("{upload:?}");
        assert!(!debug.contains("dG9wLXNlY3JldA=="));
    }
}
''')

replace_once(
    "crates/medusa-daemon/Cargo.toml",
    "[dependencies]\n",
    "[dependencies]\nbase64.workspace = true\n",
)
replace_once(
    "crates/medusa-daemon/src/lib.rs",
    '''pub use frontend_control::{
    FrontendCommandAcknowledgement, FrontendControlError, FrontendControlPlane,
    FrontendControlResult,
};
''',
    '''pub use frontend_control::{
    FrontendCommandAcknowledgement, FrontendControlError, FrontendControlPlane,
    FrontendControlResult, FrontendTransientEvent,
};
''',
)
replace_once(
    "crates/medusa-daemon/src/lib.rs",
    '''pub use protocol::{
    DAEMON_PROTOCOL_VERSION, JobRecord, JobState, Request, RequestEnvelope, Response,
    ResponseEnvelope,
};
''',
    '''pub use protocol::{
    DAEMON_PROTOCOL_VERSION, FrontendArtifactKind, FrontendArtifactUpload,
    FrontendCredentialUpdate, JobRecord, JobState, Request, RequestEnvelope, Response,
    ResponseEnvelope,
};
''',
)
