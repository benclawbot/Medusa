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

    #[must_use]
    pub const fn journal_cursor(&self) -> u64 {
        self.journal_cursor
    }
}
