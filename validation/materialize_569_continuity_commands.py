from pathlib import Path


def replace_once(source: str, old: str, new: str, label: str) -> str:
    if new in source:
        return source
    count = source.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one target, found {count}")
    return source.replace(old, new, 1)


root_path = Path("crates/medusa-session-continuity/src/root.rs")
root = root_path.read_text()
root = root.replace("pub const CURRENT_SCHEMA_VERSION: u32 = 1;", "pub const CURRENT_SCHEMA_VERSION: u32 = 2;")
root = replace_once(
    root,
    '''pub enum ClientKind {
    Tui,
    Desktop,
    Other(String),
}
''',
    '''pub enum ClientKind {
    Tui,
    Desktop,
    Telegram,
    Daemon,
    Other(String),
}
''',
    "client kinds",
)
root = replace_once(
    root,
    '''    pub attached_at_unix_ms: i64,
    pub last_seen_revision: u64,
}
''',
    '''    pub attached_at_unix_ms: i64,
    pub last_seen_revision: u64,
    #[serde(default)]
    pub journal_cursor: u64,
}
''',
    "attachment cursor",
)
root = replace_once(
    root,
    '''    ClientDetached,
    OwnershipHandedOff {
''',
    '''    ClientDetached,
    CursorAcknowledged {
        cursor: u64,
    },
    OwnershipHandedOff {
''',
    "cursor event",
)
root = replace_once(
    root,
    '''    pub expected_revision: u64,
    pub occurred_at_unix_ms: i64,
    pub event_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationRequest {
''',
    '''    pub expected_revision: u64,
    pub journal_cursor: u64,
    pub occurred_at_unix_ms: i64,
    pub event_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationRequest {
''',
    "attach cursor request",
)
root = replace_once(
    root,
    '''pub struct HandoffRequest {
    pub from_client_id: String,
    pub to_client_id: String,
    pub expected_revision: u64,
    pub occurred_at_unix_ms: i64,
    pub event_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
''',
    '''pub struct HandoffRequest {
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
''',
    "continuity command requests",
)
root = replace_once(
    root,
    '''    #[error("event sequence is invalid")]
    InvalidEventSequence,
''',
    '''    #[error("client cursor regressed from {acknowledged} to {requested}")]
    CursorRegression { acknowledged: u64, requested: u64 },
    #[error("event sequence is invalid")]
    InvalidEventSequence,
''',
    "cursor regression error",
)
root = replace_once(
    root,
    '''                    existing.client_kind = request.client_kind.clone();
                    existing.mode = request.requested_mode;
                    existing.last_seen_revision = session.revision + 1;
''',
    '''                    if session.owner_client_id.as_deref() == Some(request.client_id.as_str())
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
''',
    "existing attachment cursor",
)
root = replace_once(
    root,
    '''                        attached_at_unix_ms: request.occurred_at_unix_ms,
                        last_seen_revision: session.revision + 1,
                    });
''',
    '''                        attached_at_unix_ms: request.occurred_at_unix_ms,
                        last_seen_revision: session.revision + 1,
                        journal_cursor: request.journal_cursor,
                    });
''',
    "new attachment cursor",
)
insert_target = '''    pub fn mutate(&self, request: MutationRequest) -> Result<ApplyOutcome, ContinuityError> {
'''
commands = '''    pub fn detach(&self, request: DetachRequest) -> Result<ApplyOutcome, ContinuityError> {
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

'''
if commands not in root:
    if root.count(insert_target) != 1:
        raise SystemExit("continuity command insertion target changed")
    root = root.replace(insert_target, commands + insert_target, 1)

start = root.find("fn migrate(")
end = root.find("fn validate(", start)
if start == -1 or end == -1:
    raise SystemExit("continuity migration function changed")
root = root[:start] + '''fn migrate(mut value: serde_json::Value) -> Result<serde_json::Value, ContinuityError> {
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

''' + root[end:]

root_path.write_text(root)

session_path = Path("crates/medusa-runtime/src/attachment/session.rs")
session = session_path.read_text()
session = replace_once(
    session,
    '''use medusa_session_continuity::{AttachRequest, ContinuityError, ContinuityStore};
''',
    '''use medusa_session_continuity::{
    AttachRequest, ContinuityError, ContinuityStore, CursorAckRequest, DetachRequest,
    HandoffRequest,
};
''',
    "runtime continuity imports",
)
session = replace_once(
    session,
    '''                expected_revision: request.expected_revision,
                occurred_at_unix_ms: request.occurred_at_unix_ms,
''',
    '''                expected_revision: request.expected_revision,
                journal_cursor: request.cursor,
                occurred_at_unix_ms: request.occurred_at_unix_ms,
''',
    "runtime attach cursor",
)
session = replace_once(
    session,
    '''        let replay = replay_events(&repo, &request.session_id, request.cursor)
            .map_err(RuntimeError::agent)?;
''',
    '''        let replay_cursor = request.cursor.max(attachment.journal_cursor);
        let replay = replay_events(&repo, &request.session_id, replay_cursor)
            .map_err(RuntimeError::agent)?;
''',
    "durable replay cursor",
)
method_target = '''    /// Starts the production controller only when this client is the current owner.
'''
methods = '''    /// Acknowledges the highest canonical journal cursor observed by this client.
    pub fn acknowledge_cursor(
        &mut self,
        cursor: u64,
        occurred_at_unix_ms: i64,
        event_id: impl Into<String>,
    ) -> Result<(), RuntimeError> {
        let outcome = continuity_store(&self.repo, &self.session.id.to_string())
            .acknowledge_cursor(CursorAckRequest {
                client_id: self.client_id.clone(),
                expected_revision: self.continuity.revision,
                cursor,
                occurred_at_unix_ms,
                event_id: event_id.into(),
            })
            .map_err(RuntimeError::agent)?;
        self.continuity = outcome.session().clone();
        Ok(())
    }

    /// Hands mutable ownership to an already attached client.
    pub fn handoff(
        &mut self,
        to_client_id: impl Into<String>,
        occurred_at_unix_ms: i64,
        event_id: impl Into<String>,
    ) -> Result<(), RuntimeError> {
        self.validate_owner()?;
        let outcome = continuity_store(&self.repo, &self.session.id.to_string())
            .handoff(HandoffRequest {
                from_client_id: self.client_id.clone(),
                to_client_id: to_client_id.into(),
                expected_revision: self.continuity.revision,
                occurred_at_unix_ms,
                event_id: event_id.into(),
            })
            .map_err(RuntimeError::agent)?;
        self.continuity = outcome.session().clone();
        self.mode = self
            .continuity
            .attachments
            .iter()
            .find(|attachment| attachment.client_id == self.client_id)
            .map_or(AttachmentMode::ReadOnly, |attachment| attachment.mode);
        Ok(())
    }

    /// Detaches this client. Detaching the owner leaves the session ownerless until an explicit
    /// owner attachment or handoff occurs.
    pub fn detach(
        self,
        occurred_at_unix_ms: i64,
        event_id: impl Into<String>,
    ) -> Result<ContinuitySession, RuntimeError> {
        let outcome = continuity_store(&self.repo, &self.session.id.to_string())
            .detach(DetachRequest {
                client_id: self.client_id,
                expected_revision: self.continuity.revision,
                occurred_at_unix_ms,
                event_id: event_id.into(),
            })
            .map_err(RuntimeError::agent)?;
        Ok(outcome.session().clone())
    }

'''
if methods not in session:
    if session.count(method_target) != 1:
        raise SystemExit("runtime continuity method insertion target changed")
    session = session.replace(method_target, methods + method_target, 1)

test_marker = "fn telegram_cursor_handoff_and_detach_are_durable()"
if test_marker not in session:
    session += r'''

#[cfg(test)]
mod continuity_command_tests {
    use medusa_agent::AgentEngine;
    use medusa_config::Config;
    use medusa_core::MedusaResult;
    use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};

    use super::*;

    struct UnusedProvider;

    impl ModelProvider for UnusedProvider {
        fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {
            unreachable!("session creation does not call the provider")
        }
    }

    fn request(
        session_id: &str,
        client_id: &str,
        client_kind: ClientKind,
        requested_mode: AttachmentMode,
        expected_revision: u64,
        cursor: u64,
        event_id: &str,
    ) -> RuntimeAttachRequest {
        RuntimeAttachRequest {
            session_id: session_id.to_owned(),
            client_id: client_id.to_owned(),
            client_kind,
            requested_mode,
            expected_revision,
            cursor,
            occurred_at_unix_ms: 10_000 + i64::try_from(expected_revision).unwrap_or(i64::MAX),
            event_id: event_id.to_owned(),
        }
    }

    #[test]
    fn telegram_cursor_handoff_and_detach_are_durable() {
        let repository = tempfile::tempdir().expect("repository");
        let session = AgentEngine::new(UnusedProvider, Config::default())
            .create_session(repository.path(), "Share one transcript".to_owned())
            .expect("session");
        let session_id = session.id.to_string();
        let mut owner = RuntimeSessionAttachment::attach(
            repository.path().to_path_buf(),
            request(
                &session_id,
                "tui-owner",
                ClientKind::Tui,
                AttachmentMode::Owner,
                0,
                0,
                "attach-owner",
            ),
        )
        .expect("owner");
        let mut telegram = RuntimeSessionAttachment::attach(
            repository.path().to_path_buf(),
            request(
                &session_id,
                "telegram-42",
                ClientKind::Telegram,
                AttachmentMode::ReadOnly,
                owner.continuity.revision,
                1,
                "attach-telegram",
            ),
        )
        .expect("telegram");
        assert!(telegram.replay.is_empty());
        telegram
            .acknowledge_cursor(1, 10_002, "ack-telegram-1")
            .expect("cursor ack");
        let store = continuity_store(repository.path(), &session_id);
        let after_ack = store.load().expect("continuity");
        assert_eq!(
            after_ack
                .attachments
                .iter()
                .find(|attachment| attachment.client_id == "telegram-42")
                .expect("telegram attachment")
                .journal_cursor,
            1
        );

        owner.continuity = after_ack;
        owner
            .handoff("telegram-42", 10_003, "handoff-telegram")
            .expect("handoff");
        assert_eq!(owner.mode(), AttachmentMode::ReadOnly);
        let telegram_state = store.load().expect("continuity");
        telegram.continuity = telegram_state;
        telegram.mode = AttachmentMode::Owner;
        let detached = telegram.detach(10_004, "detach-telegram").expect("detach");
        assert_eq!(detached.owner_client_id, None);
        assert!(
            detached
                .attachments
                .iter()
                .all(|attachment| attachment.client_id != "telegram-42")
        );
    }

    #[test]
    fn cursor_acknowledgement_is_monotonic_and_idempotent() {
        let repository = tempfile::tempdir().expect("repository");
        let session = AgentEngine::new(UnusedProvider, Config::default())
            .create_session(repository.path(), "Cursor semantics".to_owned())
            .expect("session");
        let mut attachment = RuntimeSessionAttachment::attach(
            repository.path().to_path_buf(),
            request(
                &session.id.to_string(),
                "daemon-subscriber",
                ClientKind::Daemon,
                AttachmentMode::ReadOnly,
                0,
                0,
                "attach-daemon",
            ),
        )
        .expect("attach");
        attachment
            .acknowledge_cursor(1, 20_000, "ack-daemon")
            .expect("ack");
        let revision = attachment.continuity.revision;
        let store = continuity_store(repository.path(), &session.id.to_string());
        let replay = store
            .acknowledge_cursor(CursorAckRequest {
                client_id: "daemon-subscriber".to_owned(),
                expected_revision: 0,
                cursor: 1,
                occurred_at_unix_ms: 20_000,
                event_id: "ack-daemon".to_owned(),
            })
            .expect("idempotent replay");
        assert_eq!(replay.session().revision, revision);
        let error = store
            .acknowledge_cursor(CursorAckRequest {
                client_id: "daemon-subscriber".to_owned(),
                expected_revision: revision,
                cursor: 0,
                occurred_at_unix_ms: 20_001,
                event_id: "ack-regression".to_owned(),
            })
            .expect_err("cursor regression");
        assert!(error.to_string().contains("regressed"));
    }
}
'''
session_path.write_text(session)
