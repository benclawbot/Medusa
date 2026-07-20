use std::{fs, path::Path};

use medusa_core::MedusaResult;

use crate::{SessionRecallStore, SessionRecord};

/// Opens the session recall store and atomically ingests durable records left by the agent.
pub fn open_session_recall(root: impl AsRef<Path>) -> MedusaResult<SessionRecallStore> {
    let root = root.as_ref();
    let store = SessionRecallStore::new(root)?;
    let inbox = root.join(".medusa/session-recall-inbox");
    if !inbox.is_dir() {
        return Ok(store);
    }

    let mut entries = fs::read_dir(&inbox)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let record: SessionRecord = serde_json::from_slice(&fs::read(&path)?)?;
        store.upsert(&record)?;
        fs::remove_file(path)?;
    }
    Ok(store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SessionEvent, SessionSearchQuery};

    #[test]
    fn durable_inbox_is_ingested_and_removed() {
        let directory = tempfile::tempdir().expect("tempdir");
        let inbox = directory.path().join(".medusa/session-recall-inbox");
        fs::create_dir_all(&inbox).expect("inbox");
        let path = inbox.join("session.json");
        let record = SessionRecord {
            session_id: "session".to_owned(),
            parent_session_id: None,
            created_at: "2026-07-20T20:00:00Z".to_owned(),
            repository_fingerprint: "path:test".to_owned(),
            outcome: "success".to_owned(),
            events: vec![SessionEvent {
                ordinal: 0,
                kind: "objective".to_owned(),
                tool: None,
                success: Some(true),
                text: "repair update command".to_owned(),
            }],
        };
        fs::write(
            &path,
            serde_json::to_vec_pretty(&record).expect("serialize"),
        )
        .expect("write record");

        let store = open_session_recall(directory.path()).expect("open recall");
        let hits = store
            .session_search(&SessionSearchQuery {
                query: "repair update command".to_owned(),
                limit: 5,
                ..SessionSearchQuery::default()
            })
            .expect("search");
        assert_eq!(hits.len(), 1);
        assert!(!path.exists());
    }
}
