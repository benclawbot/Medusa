use std::{fs, path::Path};

use medusa_core::MedusaResult;

use crate::{SessionRecallStore, SessionRecord, support::LifecycleLock};

/// Opens the session recall store and atomically ingests durable records left by the agent.
pub fn open_session_recall(root: impl AsRef<Path>) -> MedusaResult<SessionRecallStore> {
    let root = root.as_ref();
    let inbox = root.join(".medusa/session-recall-inbox");
    fs::create_dir_all(&inbox)?;
    let _lock = LifecycleLock::acquire(&inbox)?;
    let store = SessionRecallStore::new(root)?;

    let mut entries = fs::read_dir(&inbox)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let record: SessionRecord = serde_json::from_slice(&bytes)?;
        store.upsert(&record)?;
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(store)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use super::*;
    use crate::{SessionEvent, SessionSearchQuery};

    fn record(session_id: &str) -> SessionRecord {
        SessionRecord {
            session_id: session_id.to_owned(),
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
        }
    }

    #[test]
    fn durable_inbox_is_ingested_and_removed() {
        let directory = tempfile::tempdir().expect("tempdir");
        let inbox = directory.path().join(".medusa/session-recall-inbox");
        fs::create_dir_all(&inbox).expect("inbox");
        let path = inbox.join("session.json");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&record("session")).expect("serialize"),
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

    #[test]
    fn concurrent_openers_ingest_shared_inbox_without_missing_file_races() {
        let directory = tempfile::tempdir().expect("tempdir");
        let inbox = directory.path().join(".medusa/session-recall-inbox");
        fs::create_dir_all(&inbox).expect("inbox");
        for index in 0..8 {
            fs::write(
                inbox.join(format!("session-{index}.json")),
                serde_json::to_vec_pretty(&record(&format!("session-{index}")))
                    .expect("serialize"),
            )
            .expect("write record");
        }

        let workers = 4;
        let barrier = Arc::new(Barrier::new(workers));
        std::thread::scope(|scope| {
            let handles = (0..workers)
                .map(|_| {
                    let barrier = Arc::clone(&barrier);
                    let root = directory.path().to_path_buf();
                    scope.spawn(move || {
                        barrier.wait();
                        open_session_recall(root)
                    })
                })
                .collect::<Vec<_>>();
            for handle in handles {
                handle
                    .join()
                    .expect("recall worker thread")
                    .expect("concurrent recall open");
            }
        });

        let store = SessionRecallStore::new(directory.path()).expect("store");
        for index in 0..8 {
            store
                .session_open(&format!("session-{index}"), None, 0)
                .expect("ingested session");
        }
        assert!(
            fs::read_dir(&inbox)
                .expect("inbox entries")
                .filter_map(Result::ok)
                .all(|entry| {
                    entry.path().extension().and_then(|value| value.to_str()) != Some("json")
                })
        );
    }
}
