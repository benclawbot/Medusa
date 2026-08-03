//! Canonical frontend event delivery over the durable session journal.
//!
//! Runtime workers may emit process-local wakeups and presentation hints, but user-facing
//! frontends consume the versioned protocol projected from committed journal events. This keeps
//! replay, ordering, verification, and terminal state identical across CLI and remote clients.

use std::{collections::VecDeque, path::PathBuf};

use medusa_agent::session_browser::replay_events;
use medusa_protocol::frontend::{project_event, FrontendEventEnvelope, FrontendKind};

use crate::RuntimeError;

/// Cursor-bearing projection of one authoritative runtime session for one frontend kind.
pub struct CanonicalFrontendEventStream {
    repo: PathBuf,
    frontend: FrontendKind,
    session_id: Option<String>,
    journal_cursor: u64,
    pending: VecDeque<FrontendEventEnvelope>,
}

impl CanonicalFrontendEventStream {
    #[must_use]
    pub fn new(repo: PathBuf, frontend: FrontendKind) -> Self {
        Self {
            repo,
            frontend,
            session_id: None,
            journal_cursor: 0,
            pending: VecDeque::new(),
        }
    }

    /// Resumes presentation after an acknowledged canonical journal cursor.
    pub fn resume(&mut self, session_id: impl Into<String>, after_cursor: u64) {
        self.session_id = Some(session_id.into());
        self.journal_cursor = after_cursor;
        self.pending.clear();
    }

    /// Returns the next shared frontend event, replaying committed journal state as needed.
    pub fn try_event(
        &mut self,
        session_id: &str,
    ) -> Result<Option<FrontendEventEnvelope>, RuntimeError> {
        if self.session_id.as_deref() != Some(session_id) {
            self.resume(session_id.to_owned(), 0);
        }
        if let Some(event) = self.pending.pop_front() {
            return Ok(Some(event));
        }

        let events = replay_events(&self.repo, session_id, self.journal_cursor)
            .map_err(RuntimeError::agent)?;
        for event in events {
            if event.sequence <= self.journal_cursor {
                return Err(RuntimeError::InvalidCommand(format!(
                    "frontend journal sequence {} did not advance past cursor {}",
                    event.sequence, self.journal_cursor
                )));
            }
            self.journal_cursor = event.sequence;
            if let Some(projected) = project_event(&event, event.sequence, self.frontend) {
                self.pending.push_back(projected);
            }
        }
        Ok(self.pending.pop_front())
    }

    /// Returns the last scanned canonical journal sequence, including non-presentable events.
    #[must_use]
    pub const fn journal_cursor(&self) -> u64 {
        self.journal_cursor
    }
}


#[cfg(test)]
mod tests {
    use std::path::Path;

    use medusa_agent::{AgentSession, record_session_event};
    use medusa_core::SessionId;
    use medusa_protocol::{
        Actor, EventPayload,
        frontend::{FrontendEvent, FrontendKind},
    };
    use serde_json::json;
    use tempfile::tempdir;
    use time::OffsetDateTime;

    use super::CanonicalFrontendEventStream;

    fn durable_session(repo: &Path) -> AgentSession {
        AgentSession {
            id: SessionId::new(),
            objective: "canonical frontend replay".to_owned(),
            repo: repo.to_path_buf(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            completed: false,
            turn: 0,
            plan: Vec::new(),
            pending_question: None,
            messages: Vec::new(),
            events: Vec::new(),
            evidence: Vec::new(),
            tool_artifacts: Vec::new(),
            world_model: None,
            approval_grants: Vec::new(),
            approval_receipts: Vec::new(),
            rollback_receipts: Vec::new(),
        }
    }

    #[test]
    fn stream_advances_the_canonical_cursor_through_non_presentable_events() {
        let directory = tempdir().expect("temporary repository");
        let mut session = durable_session(directory.path());
        let objective = session.objective.clone();
        record_session_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::SessionCreated { objective },
        )
        .expect("persist session creation");
        record_session_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::AssistantMessageRecorded {
                message: json!({
                    "role": "user",
                    "content": [{"type": "text", "text": "not assistant-visible"}],
                }),
            },
        )
        .expect("persist non-presentable event");
        record_session_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::RuntimeTurnFinished,
        )
        .expect("persist terminal event");

        let session_id = session.id.to_string();
        let mut stream = CanonicalFrontendEventStream::new(
            directory.path().to_path_buf(),
            FrontendKind::Headless,
        );
        let accepted = stream
            .try_event(&session_id)
            .expect("replay accepted event")
            .expect("accepted event");
        assert!(matches!(accepted.event, FrontendEvent::SubmissionAccepted));
        assert_eq!(accepted.cursor, 1);
        assert!(accepted.event_id.ends_with(":headless"));

        let finished = stream
            .try_event(&session_id)
            .expect("replay terminal event")
            .expect("terminal event");
        assert!(matches!(finished.event, FrontendEvent::TurnFinished));
        assert_eq!(finished.cursor, 3);
        assert_eq!(stream.journal_cursor(), 3);
        assert!(
            stream
                .try_event(&session_id)
                .expect("replay exhausted")
                .is_none()
        );
    }
}
