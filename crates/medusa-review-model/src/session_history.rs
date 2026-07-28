use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ReviewAuditEvent;

pub const REVIEW_HISTORY_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewSessionHistory {
    pub schema_version: u16,
    pub session_id: String,
    pub events: Vec<ReviewAuditEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewAuditExport {
    pub schema_version: u16,
    pub session_id: String,
    pub generated_at_unix_ms: i64,
    pub snapshot_ids: Vec<String>,
    pub events: Vec<ReviewAuditEvent>,
    pub resulting_repository_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
pub enum ReviewHistoryError {
    #[error("review session id must not be empty")]
    EmptySessionId,
    #[error("review history schema version is unsupported: {0}")]
    UnsupportedSchemaVersion(u16),
    #[error("review audit event id must not be empty")]
    EmptyEventId,
    #[error("review audit event snapshot id must not be empty")]
    EmptySnapshotId,
    #[error("review audit event actor must not be empty")]
    EmptyActor,
    #[error("review audit event timestamp must not be negative")]
    InvalidEventTimestamp,
    #[error("review audit event repository fingerprint must not be empty")]
    EmptyRepositoryFingerprint,
    #[error("review audit event id conflicts with an existing persisted event: {0}")]
    ConflictingEvent(String),
    #[error("review audit export timestamp must not be negative")]
    InvalidExportTimestamp,
}

impl ReviewSessionHistory {
    pub fn new(session_id: impl Into<String>) -> Result<Self, ReviewHistoryError> {
        let session_id = session_id.into();
        validate_session_id(&session_id)?;
        Ok(Self {
            schema_version: REVIEW_HISTORY_SCHEMA_VERSION,
            session_id,
            events: Vec::new(),
        })
    }

    pub fn restore(self) -> Result<Self, ReviewHistoryError> {
        validate_schema_version(self.schema_version)?;
        validate_session_id(&self.session_id)?;

        let mut ids = BTreeSet::new();
        for event in &self.events {
            validate_event(event)?;
            if !ids.insert(event.id.clone()) {
                return Err(ReviewHistoryError::ConflictingEvent(event.id.clone()));
            }
        }
        Ok(self)
    }

    pub fn append(&mut self, event: ReviewAuditEvent) -> Result<bool, ReviewHistoryError> {
        validate_schema_version(self.schema_version)?;
        validate_session_id(&self.session_id)?;
        validate_event(&event)?;

        if let Some(existing) = self.events.iter().find(|existing| existing.id == event.id) {
            if existing == &event {
                return Ok(false);
            }
            return Err(ReviewHistoryError::ConflictingEvent(event.id));
        }

        self.events.push(event);
        Ok(true)
    }

    #[must_use]
    pub fn events_for_snapshot(&self, snapshot_id: &str) -> Vec<&ReviewAuditEvent> {
        self.events
            .iter()
            .filter(|event| event.snapshot_id == snapshot_id)
            .collect()
    }

    pub fn export(
        &self,
        generated_at_unix_ms: i64,
    ) -> Result<ReviewAuditExport, ReviewHistoryError> {
        validate_schema_version(self.schema_version)?;
        validate_session_id(&self.session_id)?;
        if generated_at_unix_ms < 0 {
            return Err(ReviewHistoryError::InvalidExportTimestamp);
        }

        let mut events = self.events.clone();
        for event in &events {
            validate_event(event)?;
        }
        events.sort_by(|left, right| {
            left.occurred_at_unix_ms
                .cmp(&right.occurred_at_unix_ms)
                .then_with(|| left.id.cmp(&right.id))
        });

        let snapshot_ids = events
            .iter()
            .map(|event| event.snapshot_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let resulting_repository_fingerprint = events
            .last()
            .map(|event| event.repository_fingerprint_after.clone());

        Ok(ReviewAuditExport {
            schema_version: REVIEW_HISTORY_SCHEMA_VERSION,
            session_id: self.session_id.clone(),
            generated_at_unix_ms,
            snapshot_ids,
            events,
            resulting_repository_fingerprint,
        })
    }
}

fn validate_schema_version(schema_version: u16) -> Result<(), ReviewHistoryError> {
    if schema_version != REVIEW_HISTORY_SCHEMA_VERSION {
        return Err(ReviewHistoryError::UnsupportedSchemaVersion(schema_version));
    }
    Ok(())
}

fn validate_session_id(session_id: &str) -> Result<(), ReviewHistoryError> {
    if session_id.trim().is_empty() {
        return Err(ReviewHistoryError::EmptySessionId);
    }
    Ok(())
}

fn validate_event(event: &ReviewAuditEvent) -> Result<(), ReviewHistoryError> {
    if event.id.trim().is_empty() {
        return Err(ReviewHistoryError::EmptyEventId);
    }
    if event.snapshot_id.trim().is_empty() {
        return Err(ReviewHistoryError::EmptySnapshotId);
    }
    if event.actor.trim().is_empty() {
        return Err(ReviewHistoryError::EmptyActor);
    }
    if event.occurred_at_unix_ms < 0 {
        return Err(ReviewHistoryError::InvalidEventTimestamp);
    }
    if event.repository_fingerprint_before.trim().is_empty()
        || event.repository_fingerprint_after.trim().is_empty()
    {
        return Err(ReviewHistoryError::EmptyRepositoryFingerprint);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{ReviewAuditDecision, ReviewAuditScope};

    use super::*;

    fn event(id: &str, snapshot_id: &str, occurred_at_unix_ms: i64) -> ReviewAuditEvent {
        ReviewAuditEvent {
            id: id.into(),
            snapshot_id: snapshot_id.into(),
            scope: ReviewAuditScope::File {
                path: "src/lib.rs".into(),
            },
            decision: ReviewAuditDecision::Accepted,
            actor: "user:alice".into(),
            occurred_at_unix_ms,
            repository_fingerprint_before: format!("repo-before-{id}"),
            repository_fingerprint_after: format!("repo-after-{id}"),
        }
    }

    #[test]
    fn appends_events_and_treats_identical_replay_as_idempotent() {
        let mut history = ReviewSessionHistory::new("session-1").unwrap();
        let item = event("event-1", "snapshot-1", 20);

        assert!(history.append(item.clone()).unwrap());
        assert!(!history.append(item).unwrap());
        assert_eq!(history.events.len(), 1);
    }

    #[test]
    fn rejects_conflicting_event_with_reused_identity() {
        let mut history = ReviewSessionHistory::new("session-1").unwrap();
        history.append(event("event-1", "snapshot-1", 20)).unwrap();
        let mut conflicting = event("event-1", "snapshot-1", 20);
        conflicting.actor = "user:bob".into();

        assert_eq!(
            history.append(conflicting),
            Err(ReviewHistoryError::ConflictingEvent("event-1".into()))
        );
    }

    #[test]
    fn export_is_deterministic_and_reports_resulting_repository_state() {
        let mut history = ReviewSessionHistory::new("session-1").unwrap();
        history
            .append(event("event-later", "snapshot-2", 30))
            .unwrap();
        history
            .append(event("event-earlier", "snapshot-1", 10))
            .unwrap();

        let export = history.export(40).unwrap();
        assert_eq!(
            export
                .events
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["event-earlier", "event-later"]
        );
        assert_eq!(export.snapshot_ids, vec!["snapshot-1", "snapshot-2"]);
        assert_eq!(
            export.resulting_repository_fingerprint.as_deref(),
            Some("repo-after-event-later")
        );
    }

    #[test]
    fn restore_rejects_duplicate_or_invalid_persisted_history() {
        let duplicated = ReviewSessionHistory {
            schema_version: REVIEW_HISTORY_SCHEMA_VERSION,
            session_id: "session-1".into(),
            events: vec![
                event("event-1", "snapshot-1", 10),
                event("event-1", "snapshot-1", 10),
            ],
        };
        assert_eq!(
            duplicated.restore(),
            Err(ReviewHistoryError::ConflictingEvent("event-1".into()))
        );

        let unsupported = ReviewSessionHistory {
            schema_version: REVIEW_HISTORY_SCHEMA_VERSION + 1,
            session_id: "session-1".into(),
            events: vec![],
        };
        assert_eq!(
            unsupported.restore(),
            Err(ReviewHistoryError::UnsupportedSchemaVersion(
                REVIEW_HISTORY_SCHEMA_VERSION + 1
            ))
        );
    }

    #[test]
    fn filters_history_by_review_snapshot_identity() {
        let mut history = ReviewSessionHistory::new("session-1").unwrap();
        history.append(event("event-1", "snapshot-1", 10)).unwrap();
        history.append(event("event-2", "snapshot-2", 20)).unwrap();

        let events = history.events_for_snapshot("snapshot-2");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "event-2");
    }
}
