use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const CURRENT_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    Tui,
    Desktop,
    Telegram,
    Daemon,
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentMode {
    Owner,
    ReadOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientAttachment {
    pub client_id: String,
    pub client_kind: ClientKind,
    pub mode: AttachmentMode,
    pub attached_at_unix_ms: i64,
    pub last_seen_revision: u64,
    #[serde(default)]
    pub journal_cursor: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEvent {
    pub id: String,
    pub sequence: u64,
    pub client_id: String,
    pub occurred_at_unix_ms: i64,
    pub kind: SessionEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEventKind {
    ClientAttached {
        client_kind: ClientKind,
        mode: AttachmentMode,
    },
    ClientDetached,
    CursorAcknowledged {
        cursor: u64,
    },
    OwnershipHandedOff {
        from_client_id: String,
        to_client_id: String,
    },
    TaskStateChanged {
        state: String,
    },
    VerificationRecorded {
        check_id: String,
        outcome: String,
    },
    ApprovalRecorded {
        approval_id: String,
        decision: String,
    },
    CheckpointRecorded {
        checkpoint_id: String,
    },
    RecoveryStateChanged {
        state: String,
    },
    CompletionRecorded {
        status: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AuthoritativeTaskState {
    pub plan_state: Option<String>,
    pub active_step: Option<String>,
    pub attention_required: bool,
    pub approvals: Vec<String>,
    pub checkpoints: Vec<String>,
    pub recovery_state: Option<String>,
    pub verification_evidence: Vec<String>,
    pub file_changes: Vec<String>,
    pub completion_status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuitySession {
    pub schema_version: u32,
    pub session_id: String,
    pub revision: u64,
    pub owner_client_id: Option<String>,
    pub attachments: Vec<ClientAttachment>,
    pub task: AuthoritativeTaskState,
    pub events: Vec<SessionEvent>,
}

impl ContinuitySession {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            session_id: session_id.into(),
            revision: 0,
            owner_client_id: None,
            attachments: Vec::new(),
            task: AuthoritativeTaskState::default(),
            events: Vec::new(),
        }
    }

    pub fn event(&self, event_id: &str) -> Option<&SessionEvent> {
        self.events.iter().find(|event| event.id == event_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachRequest {
    pub client_id: String,
    pub client_kind: ClientKind,
    pub requested_mode: AttachmentMode,
    pub expected_revision: u64,
    pub journal_cursor: u64,
    pub occurred_at_unix_ms: i64,
    pub event_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationRequest {
    pub client_id: String,
    pub expected_revision: u64,
    pub occurred_at_unix_ms: i64,
    pub event_id: String,
    pub event: SessionEventKind,
    pub task: AuthoritativeTaskState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffRequest {
    pub from_client_id: String,
    pub to_client_id: String,
    pub expected_revision: u64,
    pub occurred_at_unix_ms: i64,
    pub event_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachRequest {
    pub client_id: String,
    pub expected_revision: u64,
    pub occurred_at_unix_ms: i64,
    pub event_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorAckRequest {
    pub client_id: String,
    pub expected_revision: u64,
    pub cursor: u64,
    pub occurred_at_unix_ms: i64,
    pub event_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    Applied(ContinuitySession),
    Replayed(ContinuitySession),
}

impl ApplyOutcome {
    pub fn session(&self) -> &ContinuitySession {
        match self {
            Self::Applied(session) | Self::Replayed(session) => session,
        }
    }
}

#[derive(Debug, Error)]
pub enum ContinuityError {
    #[error("session I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("session JSON is invalid: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("unsupported session schema version {found}; current version is {current}")]
    UnsupportedSchema { found: u32, current: u32 },
    #[error("session revision is stale: expected {expected}, authoritative revision is {actual}")]
    StaleRevision { expected: u64, actual: u64 },
    #[error("session is currently owned by client {owner}")]
    OwnershipConflict { owner: String },
    #[error("client {client_id} is not attached")]
    ClientNotAttached { client_id: String },
    #[error("client {client_id} is attached read-only")]
    ReadOnlyClient { client_id: String },
    #[error("client {client_id} does not own the session")]
    NotOwner { client_id: String },
    #[error("handoff target {client_id} must already be attached")]
    HandoffTargetNotAttached { client_id: String },
    #[error("event id {event_id} was reused with conflicting content")]
    ConflictingReplay { event_id: String },
    #[error("client cursor regressed from {acknowledged} to {requested}")]
    CursorRegression { acknowledged: u64, requested: u64 },
    #[error("event sequence is invalid")]
    InvalidEventSequence,
    #[error("session attachment state is inconsistent")]
    InvalidAttachmentState,
}

#[derive(Debug, Clone)]
pub struct ContinuityStore {
    path: PathBuf,
}

impl ContinuityStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn create(
        &self,
        session_id: impl Into<String>,
    ) -> Result<ContinuitySession, ContinuityError> {
        let session = ContinuitySession::new(session_id);
        self.persist(&session)?;
        Ok(session)
    }

    pub fn load(&self) -> Result<ContinuitySession, ContinuityError> {
        let bytes = fs::read(&self.path)?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        let migrated = migrate(value)?;
        let session: ContinuitySession = serde_json::from_value(migrated)?;
        validate(&session)?;
        Ok(session)
    }

    pub fn attach(&self, request: AttachRequest) -> Result<ApplyOutcome, ContinuityError> {
        self.update(
            request.expected_revision,
            &request.event_id,
            |session| {
                if let Some(existing) = session
                    .attachments
                    .iter_mut()
                    .find(|attachment| attachment.client_id == request.client_id)
                {
                    if request.requested_mode == AttachmentMode::Owner {
                        match session.owner_client_id.as_deref() {
                            None => session.owner_client_id = Some(request.client_id.clone()),
                            Some(owner) if owner == request.client_id => {}
                            Some(owner) => {
                                return Err(ContinuityError::OwnershipConflict {
                                    owner: owner.to_owned(),
                                });
                            }
                        }
                    }
                    if session.owner_client_id.as_deref() == Some(request.client_id.as_str())
                        && request.requested_mode == AttachmentMode::ReadOnly
                    {
                        return Err(ContinuityError::NotOwner {
                            client_id: request.client_id.clone(),
                        });
                    }
                    existing.client_kind = request.client_kind.clone();
                    existing.mode = request.requested_mode;
                    existing.last_seen_revision = session.revision + 1;
                    existing.journal_cursor = existing.journal_cursor.max(request.journal_cursor);
                } else {
                    if request.requested_mode == AttachmentMode::Owner {
                        if let Some(owner) = &session.owner_client_id {
                            return Err(ContinuityError::OwnershipConflict {
                                owner: owner.clone(),
                            });
                        }
                        session.owner_client_id = Some(request.client_id.clone());
                    }
                    session.attachments.push(ClientAttachment {
                        client_id: request.client_id.clone(),
                        client_kind: request.client_kind.clone(),
                        mode: request.requested_mode,
                        attached_at_unix_ms: request.occurred_at_unix_ms,
                        last_seen_revision: session.revision + 1,
                        journal_cursor: request.journal_cursor,
                    });
                }
                Ok(SessionEventKind::ClientAttached {
                    client_kind: request.client_kind,
                    mode: request.requested_mode,
                })
            },
            &request.client_id,
            request.occurred_at_unix_ms,
        )
    }

    pub fn handoff(&self, request: HandoffRequest) -> Result<ApplyOutcome, ContinuityError> {
        self.update(
            request.expected_revision,
            &request.event_id,
            |session| {
                if session.owner_client_id.as_deref() != Some(request.from_client_id.as_str()) {
                    return Err(ContinuityError::NotOwner {
                        client_id: request.from_client_id.clone(),
                    });
                }
                let Some(target) = session
                    .attachments
                    .iter_mut()
                    .find(|attachment| attachment.client_id == request.to_client_id)
                else {
                    return Err(ContinuityError::HandoffTargetNotAttached {
                        client_id: request.to_client_id.clone(),
                    });
                };
                target.mode = AttachmentMode::Owner;
                target.last_seen_revision = session.revision + 1;
                if let Some(source) = session
                    .attachments
                    .iter_mut()
                    .find(|attachment| attachment.client_id == request.from_client_id)
                {
                    source.mode = AttachmentMode::ReadOnly;
                    source.last_seen_revision = session.revision + 1;
                }
                session.owner_client_id = Some(request.to_client_id.clone());
                Ok(SessionEventKind::OwnershipHandedOff {
                    from_client_id: request.from_client_id.clone(),
                    to_client_id: request.to_client_id,
                })
            },
            &request.from_client_id,
            request.occurred_at_unix_ms,
        )
    }

    pub fn detach(&self, request: DetachRequest) -> Result<ApplyOutcome, ContinuityError> {
        self.update(
            request.expected_revision,
            &request.event_id,
            |session| {
                let position = session
                    .attachments
                    .iter()
                    .position(|attachment| attachment.client_id == request.client_id)
                    .ok_or_else(|| ContinuityError::ClientNotAttached {
                        client_id: request.client_id.clone(),
                    })?;
                if session.owner_client_id.as_deref() == Some(request.client_id.as_str()) {
                    session.owner_client_id = None;
                }
                session.attachments.remove(position);
                Ok(SessionEventKind::ClientDetached)
            },
            &request.client_id,
            request.occurred_at_unix_ms,
        )
    }

    pub fn acknowledge_cursor(
        &self,
        request: CursorAckRequest,
    ) -> Result<ApplyOutcome, ContinuityError> {
        self.update(
            request.expected_revision,
            &request.event_id,
            |session| {
                let attachment = session
                    .attachments
                    .iter_mut()
                    .find(|attachment| attachment.client_id == request.client_id)
                    .ok_or_else(|| ContinuityError::ClientNotAttached {
                        client_id: request.client_id.clone(),
                    })?;
                if request.cursor < attachment.journal_cursor {
                    return Err(ContinuityError::CursorRegression {
                        acknowledged: attachment.journal_cursor,
                        requested: request.cursor,
                    });
                }
                attachment.journal_cursor = request.cursor;
                attachment.last_seen_revision = session.revision + 1;
                Ok(SessionEventKind::CursorAcknowledged {
                    cursor: request.cursor,
                })
            },
            &request.client_id,
            request.occurred_at_unix_ms,
        )
    }

    pub fn mutate(&self, request: MutationRequest) -> Result<ApplyOutcome, ContinuityError> {
        self.update(
            request.expected_revision,
            &request.event_id,
            |session| {
                let attachment = session
                    .attachments
                    .iter()
                    .find(|attachment| attachment.client_id == request.client_id)
                    .ok_or_else(|| ContinuityError::ClientNotAttached {
                        client_id: request.client_id.clone(),
                    })?;
                if attachment.mode == AttachmentMode::ReadOnly {
                    return Err(ContinuityError::ReadOnlyClient {
                        client_id: request.client_id.clone(),
                    });
                }
                if session.owner_client_id.as_deref() != Some(request.client_id.as_str()) {
                    return Err(ContinuityError::NotOwner {
                        client_id: request.client_id.clone(),
                    });
                }
                session.task = request.task;
                Ok(request.event)
            },
            &request.client_id,
            request.occurred_at_unix_ms,
        )
    }

    fn update<F>(
        &self,
        expected_revision: u64,
        event_id: &str,
        apply: F,
        client_id: &str,
        occurred_at_unix_ms: i64,
    ) -> Result<ApplyOutcome, ContinuityError>
    where
        F: FnOnce(&mut ContinuitySession) -> Result<SessionEventKind, ContinuityError>,
    {
        let mut session = self.load()?;
        if let Some(existing) = session.event(event_id) {
            if existing.client_id == client_id
                && existing.occurred_at_unix_ms == occurred_at_unix_ms
            {
                return Ok(ApplyOutcome::Replayed(session));
            }
            return Err(ContinuityError::ConflictingReplay {
                event_id: event_id.to_owned(),
            });
        }
        if session.revision != expected_revision {
            return Err(ContinuityError::StaleRevision {
                expected: expected_revision,
                actual: session.revision,
            });
        }

        let kind = apply(&mut session)?;
        let sequence = session
            .events
            .last()
            .map_or(0, |event| event.sequence.saturating_add(1));
        session.events.push(SessionEvent {
            id: event_id.to_owned(),
            sequence,
            client_id: client_id.to_owned(),
            occurred_at_unix_ms,
            kind,
        });
        session.revision = session.revision.saturating_add(1);
        for attachment in &mut session.attachments {
            if attachment.client_id == client_id {
                attachment.last_seen_revision = session.revision;
            }
        }
        validate(&session)?;
        self.persist(&session)?;
        Ok(ApplyOutcome::Applied(session))
    }

    fn persist(&self, session: &ContinuitySession) -> Result<(), ContinuityError> {
        validate(session)?;
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let temp = self.path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(session)?;
        {
            let mut file = fs::File::create(&temp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        fs::rename(&temp, &self.path)?;
        if let Ok(directory) = fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    }
}

fn migrate(mut value: serde_json::Value) -> Result<serde_json::Value, ContinuityError> {
    let version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    if version == 0 {
        let object = value.as_object_mut().ok_or_else(|| {
            serde_json::Error::io(io::Error::new(
                io::ErrorKind::InvalidData,
                "session root must be an object",
            ))
        })?;
        object
            .entry("revision")
            .or_insert_with(|| serde_json::Value::from(0));
        object
            .entry("owner_client_id")
            .or_insert(serde_json::Value::Null);
        object
            .entry("attachments")
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
        object.entry("task").or_insert_with(|| {
            serde_json::json!({
                "plan_state": null,
                "active_step": null,
                "attention_required": false,
                "approvals": [],
                "checkpoints": [],
                "recovery_state": null,
                "verification_evidence": [],
                "file_changes": [],
                "completion_status": null,
            })
        });
        object
            .entry("events")
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    } else if version > u64::from(CURRENT_SCHEMA_VERSION) {
        return Err(ContinuityError::UnsupportedSchema {
            found: u32::try_from(version).unwrap_or(u32::MAX),
            current: CURRENT_SCHEMA_VERSION,
        });
    }

    let object = value.as_object_mut().ok_or_else(|| {
        serde_json::Error::io(io::Error::new(
            io::ErrorKind::InvalidData,
            "session root must be an object",
        ))
    })?;
    if let Some(attachments) = object
        .get_mut("attachments")
        .and_then(serde_json::Value::as_array_mut)
    {
        for attachment in attachments {
            let attachment = attachment.as_object_mut().ok_or_else(|| {
                serde_json::Error::io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "session attachment must be an object",
                ))
            })?;
            attachment
                .entry("journal_cursor")
                .or_insert_with(|| serde_json::Value::from(0));
        }
    }
    object.insert(
        "schema_version".to_owned(),
        serde_json::Value::from(CURRENT_SCHEMA_VERSION),
    );
    Ok(value)
}

fn validate(session: &ContinuitySession) -> Result<(), ContinuityError> {
    if session.schema_version != CURRENT_SCHEMA_VERSION {
        return Err(ContinuityError::UnsupportedSchema {
            found: session.schema_version,
            current: CURRENT_SCHEMA_VERSION,
        });
    }
    for (expected, event) in session.events.iter().enumerate() {
        if event.sequence != expected as u64 {
            return Err(ContinuityError::InvalidEventSequence);
        }
    }
    let owners = session
        .attachments
        .iter()
        .filter(|attachment| attachment.mode == AttachmentMode::Owner)
        .collect::<Vec<_>>();
    match (&session.owner_client_id, owners.as_slice()) {
        (None, []) => {}
        (Some(owner), [attachment]) if attachment.client_id == *owner => {}
        _ => return Err(ContinuityError::InvalidAttachmentState),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(step: &str, status: Option<&str>) -> AuthoritativeTaskState {
        AuthoritativeTaskState {
            plan_state: Some("running".to_owned()),
            active_step: Some(step.to_owned()),
            attention_required: false,
            approvals: vec!["approve-network:granted".to_owned()],
            checkpoints: vec!["checkpoint-1".to_owned()],
            recovery_state: Some("recoverable".to_owned()),
            verification_evidence: vec!["cargo-test:passed".to_owned()],
            file_changes: vec!["src/lib.rs".to_owned()],
            completion_status: status.map(str::to_owned),
        }
    }

    fn attach(
        store: &ContinuityStore,
        revision: u64,
        id: &str,
        kind: ClientKind,
        mode: AttachmentMode,
        event: &str,
        at: i64,
    ) -> ContinuitySession {
        store
            .attach(AttachRequest {
                client_id: id.to_owned(),
                client_kind: kind,
                requested_mode: mode,
                expected_revision: revision,
                journal_cursor: 0,
                occurred_at_unix_ms: at,
                event_id: event.to_owned(),
            })
            .expect("attach")
            .session()
            .clone()
    }

    #[test]
    fn cross_client_handoff_preserves_identical_authoritative_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ContinuityStore::new(temp.path().join("session.json"));
        let initial = store.create("session-1").expect("create");

        let tui = attach(
            &store,
            initial.revision,
            "tui-1",
            ClientKind::Tui,
            AttachmentMode::Owner,
            "attach-tui",
            1,
        );
        let after_tui = store
            .mutate(MutationRequest {
                client_id: "tui-1".to_owned(),
                expected_revision: tui.revision,
                occurred_at_unix_ms: 2,
                event_id: "tui-progress".to_owned(),
                event: SessionEventKind::TaskStateChanged {
                    state: "step-1".to_owned(),
                },
                task: task("step-1", None),
            })
            .expect("tui mutation")
            .session()
            .clone();

        let desktop = attach(
            &store,
            after_tui.revision,
            "desktop-1",
            ClientKind::Desktop,
            AttachmentMode::ReadOnly,
            "attach-desktop",
            3,
        );
        let handed = store
            .handoff(HandoffRequest {
                from_client_id: "tui-1".to_owned(),
                to_client_id: "desktop-1".to_owned(),
                expected_revision: desktop.revision,
                occurred_at_unix_ms: 4,
                event_id: "handoff-desktop".to_owned(),
            })
            .expect("handoff")
            .session()
            .clone();
        let after_desktop = store
            .mutate(MutationRequest {
                client_id: "desktop-1".to_owned(),
                expected_revision: handed.revision,
                occurred_at_unix_ms: 5,
                event_id: "desktop-progress".to_owned(),
                event: SessionEventKind::VerificationRecorded {
                    check_id: "cargo-test".to_owned(),
                    outcome: "passed".to_owned(),
                },
                task: task("step-2", None),
            })
            .expect("desktop mutation")
            .session()
            .clone();
        let back_to_tui = store
            .handoff(HandoffRequest {
                from_client_id: "desktop-1".to_owned(),
                to_client_id: "tui-1".to_owned(),
                expected_revision: after_desktop.revision,
                occurred_at_unix_ms: 6,
                event_id: "handoff-tui".to_owned(),
            })
            .expect("handoff back")
            .session()
            .clone();
        let completed = store
            .mutate(MutationRequest {
                client_id: "tui-1".to_owned(),
                expected_revision: back_to_tui.revision,
                occurred_at_unix_ms: 7,
                event_id: "complete".to_owned(),
                event: SessionEventKind::CompletionRecorded {
                    status: "verified".to_owned(),
                },
                task: task("done", Some("verified")),
            })
            .expect("complete")
            .session()
            .clone();

        let desktop_view = store.load().expect("desktop reload");
        let tui_view = store.load().expect("tui reload");
        assert_eq!(desktop_view, tui_view);
        assert_eq!(desktop_view, completed);
        assert_eq!(desktop_view.owner_client_id.as_deref(), Some("tui-1"));
        assert_eq!(
            desktop_view.task.completion_status.as_deref(),
            Some("verified")
        );
        assert_eq!(
            desktop_view
                .events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            (0..desktop_view.events.len() as u64).collect::<Vec<_>>()
        );
    }

    #[test]
    fn stale_and_read_only_clients_cannot_mutate() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ContinuityStore::new(temp.path().join("session.json"));
        store.create("session-1").expect("create");
        let owner = attach(
            &store,
            0,
            "tui",
            ClientKind::Tui,
            AttachmentMode::Owner,
            "a",
            1,
        );
        let observer = attach(
            &store,
            owner.revision,
            "desktop",
            ClientKind::Desktop,
            AttachmentMode::ReadOnly,
            "b",
            2,
        );

        let read_only = store.mutate(MutationRequest {
            client_id: "desktop".to_owned(),
            expected_revision: observer.revision,
            occurred_at_unix_ms: 3,
            event_id: "read-only-write".to_owned(),
            event: SessionEventKind::TaskStateChanged {
                state: "bad".to_owned(),
            },
            task: task("bad", None),
        });
        assert!(matches!(
            read_only,
            Err(ContinuityError::ReadOnlyClient { .. })
        ));

        let first = store
            .mutate(MutationRequest {
                client_id: "tui".to_owned(),
                expected_revision: observer.revision,
                occurred_at_unix_ms: 4,
                event_id: "owner-write".to_owned(),
                event: SessionEventKind::TaskStateChanged {
                    state: "good".to_owned(),
                },
                task: task("good", None),
            })
            .expect("owner write")
            .session()
            .clone();
        let stale = store.mutate(MutationRequest {
            client_id: "tui".to_owned(),
            expected_revision: observer.revision,
            occurred_at_unix_ms: 5,
            event_id: "stale-write".to_owned(),
            event: SessionEventKind::TaskStateChanged {
                state: "stale".to_owned(),
            },
            task: task("stale", None),
        });
        assert!(matches!(
            stale,
            Err(ContinuityError::StaleRevision { expected, actual })
                if expected == observer.revision && actual == first.revision
        ));
    }

    #[test]
    fn duplicate_replay_is_idempotent_and_conflicting_reuse_fails() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ContinuityStore::new(temp.path().join("session.json"));
        store.create("session-1").expect("create");
        let request = AttachRequest {
            client_id: "tui".to_owned(),
            client_kind: ClientKind::Tui,
            requested_mode: AttachmentMode::Owner,
            expected_revision: 0,
            journal_cursor: 0,
            occurred_at_unix_ms: 1,
            event_id: "attach".to_owned(),
        };
        let applied = store.attach(request.clone()).expect("applied");
        let replayed = store.attach(request).expect("replayed");
        assert!(matches!(applied, ApplyOutcome::Applied(_)));
        assert!(matches!(replayed, ApplyOutcome::Replayed(_)));
        assert_eq!(applied.session(), replayed.session());

        let conflict = store.attach(AttachRequest {
            client_id: "desktop".to_owned(),
            client_kind: ClientKind::Desktop,
            requested_mode: AttachmentMode::ReadOnly,
            expected_revision: applied.session().revision,
            journal_cursor: 0,
            occurred_at_unix_ms: 2,
            event_id: "attach".to_owned(),
        });
        assert!(matches!(
            conflict,
            Err(ContinuityError::ConflictingReplay { .. })
        ));
    }

    #[test]
    fn legacy_schema_migrates_and_partial_write_does_not_replace_authoritative_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("session.json");
        fs::write(
            &path,
            r#"{"session_id":"legacy","task":{"plan_state":null,"active_step":null,"attention_required":false,"approvals":[],"checkpoints":[],"recovery_state":null,"verification_evidence":[],"file_changes":[],"completion_status":null}}"#,
        )
        .expect("legacy write");
        let store = ContinuityStore::new(&path);
        let migrated = store.load().expect("migration");
        assert_eq!(migrated.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(migrated.revision, 0);

        fs::write(path.with_extension("json.tmp"), b"{partial").expect("partial temp");
        let authoritative = store.load().expect("authoritative remains readable");
        assert_eq!(authoritative.session_id, "legacy");
    }

    #[test]
    fn two_clients_cannot_both_open_as_owner() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ContinuityStore::new(temp.path().join("session.json"));
        store.create("session-1").expect("create");
        let owner = attach(
            &store,
            0,
            "tui",
            ClientKind::Tui,
            AttachmentMode::Owner,
            "a",
            1,
        );
        let second = store.attach(AttachRequest {
            client_id: "desktop".to_owned(),
            client_kind: ClientKind::Desktop,
            requested_mode: AttachmentMode::Owner,
            expected_revision: owner.revision,
            journal_cursor: 0,
            occurred_at_unix_ms: 2,
            event_id: "b".to_owned(),
        });
        assert!(matches!(
            second,
            Err(ContinuityError::OwnershipConflict { owner }) if owner == "tui"
        ));
    }
}
