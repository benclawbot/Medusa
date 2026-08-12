use std::{path::Path, time::Duration};

use medusa_core::{MedusaResult, SessionId};
use rusqlite::{Connection, params};

use crate::support::{LifecycleLock, durable_remove, invalid, sql_error};

/// Removes every Medusa-owned session-recall copy for one disposed session.
///
/// The inbox lock serializes deletion against ingestion. The SQLite delete is idempotent, so a
/// crash/retry cannot resurrect an already disposed recall record. The database itself is not
/// created merely to record an absence.
pub fn delete_session_recall(root: impl AsRef<Path>, session_id: &str) -> MedusaResult<()> {
    let session_id = SessionId::parse(session_id).map_err(invalid)?;
    let root = root.as_ref();
    let inbox = root.join(".medusa/session-recall-inbox");
    let _lock = LifecycleLock::acquire(&inbox)?;

    durable_remove(&inbox.join(format!("{session_id}.json")))?;

    let database = root.join(".medusa/session-recall.sqlite3");
    if !database.is_file() {
        return Ok(());
    }
    let connection = Connection::open(database).map_err(sql_error)?;
    connection
        .busy_timeout(Duration::from_secs(30))
        .map_err(sql_error)?;
    connection
        .execute(
            "DELETE FROM session_recall WHERE session_id = ?1",
            params![session_id.to_string()],
        )
        .map_err(sql_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{SessionEvent, SessionRecallStore, SessionRecord, SessionSearchQuery};

    fn record(session_id: &str) -> SessionRecord {
        SessionRecord {
            session_id: session_id.to_owned(),
            parent_session_id: None,
            created_at: "2026-08-11T20:00:00Z".to_owned(),
            repository_fingerprint: "sha256:recall-delete".to_owned(),
            outcome: "success".to_owned(),
            events: vec![SessionEvent {
                ordinal: 0,
                kind: "objective".to_owned(),
                tool: None,
                success: Some(true),
                text: "PRIVATE_RECALL_DELETE_MARKER".to_owned(),
            }],
        }
    }

    #[test]
    fn disposed_session_cannot_be_opened_or_searched_from_recall_store() {
        let directory = tempfile::tempdir().expect("tempdir");
        let session_id = SessionId::new().to_string();
        let store = SessionRecallStore::new(directory.path()).expect("store");
        store.upsert(&record(&session_id)).expect("upsert");

        let inbox = directory.path().join(".medusa/session-recall-inbox");
        fs::create_dir_all(&inbox).expect("inbox");
        let inbox_record = inbox.join(format!("{session_id}.json"));
        fs::write(&inbox_record, b"stale recall copy").expect("stale inbox copy");
        assert!(store.session_open(&session_id, None, 0).is_ok());

        delete_session_recall(directory.path(), &session_id).expect("delete recall");
        assert!(!inbox_record.exists());
        assert!(store.session_open(&session_id, None, 0).is_err());
        assert!(
            store
                .session_search(&SessionSearchQuery {
                    query: "PRIVATE_RECALL_DELETE_MARKER".to_owned(),
                    limit: 10,
                    ..SessionSearchQuery::default()
                })
                .expect("search")
                .is_empty()
        );

        delete_session_recall(directory.path(), &session_id).expect("idempotent delete");
    }
}
