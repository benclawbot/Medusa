use std::{fs, path::Path};

use medusa_core::MedusaResult;
use tracing::warn;

use crate::{SessionRecallStore, SessionRecord, support::LifecycleLock};

/// Opens the session recall store and atomically ingests durable records left by the agent.
///
/// A single corrupt record must not abort the whole inbox: per-file failures are
/// quarantined aside (so they don't poison every future open) and ingestion
/// continues with the remaining files. Non-JSON files are logged and left in
/// place instead of being silently skipped.
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
            warn!(
                path = %path.display(),
                "ignoring non-JSON file in session recall inbox"
            );
            continue;
        }
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let record: SessionRecord = match serde_json::from_slice(&bytes) {
            Ok(record) => record,
            Err(error) => {
                quarantine_bad_record(&path);
                warn!(
                    path = %path.display(),
                    %error,
                    "skipping unparseable session recall record"
                );
                continue;
            }
        };
        if let Err(error) = store.upsert(&record) {
            quarantine_bad_record(&path);
            warn!(
                path = %path.display(),
                %error,
                "skipping session recall record that failed ingestion"
            );
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(store)
}

/// Moves an un-ingestible inbox file aside so it can't poison future opens.
/// Best-effort: if the rename fails the file is left in place and will be
/// skipped (with a warning) again on the next open.
fn quarantine_bad_record(path: &Path) {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0);
    let mut quarantined = path.as_os_str().to_owned();
    // Timestamped suffix: a repeat offender must not overwrite the previously
    // quarantined evidence.
    quarantined.push(format!(".quarantined-{stamp}-{}", std::process::id()));
    if let Err(error) = fs::rename(path, Path::new(&quarantined)) {
        warn!(
            path = %path.display(),
            %error,
            "could not quarantine bad session recall record"
        );
    }
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
    fn one_bad_record_does_not_abort_the_inbox() {
        let directory = tempfile::tempdir().expect("tempdir");
        let inbox = directory.path().join(".medusa/session-recall-inbox");
        fs::create_dir_all(&inbox).expect("inbox");
        let good = inbox.join("good.json");
        fs::write(
            &good,
            serde_json::to_vec_pretty(&record("good-session")).expect("serialize"),
        )
        .expect("write record");
        let bad = inbox.join("bad.json");
        fs::write(&bad, b"{not valid json").expect("write bad record");
        let noted = inbox.join("README.txt");
        fs::write(&noted, b"operator note").expect("write note");

        let store = open_session_recall(directory.path()).expect("open recall");
        let hits = store
            .session_search(&SessionSearchQuery {
                query: "repair update command".to_owned(),
                limit: 5,
                ..SessionSearchQuery::default()
            })
            .expect("search");
        assert_eq!(hits.len(), 1);
        assert!(!good.exists(), "ingested record must be removed");
        assert!(!bad.exists(), "bad record must be quarantined aside");
        let quarantined: Vec<_> = fs::read_dir(&inbox)
            .expect("inbox entries")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("bad.json.quarantined-"))
            })
            .collect();
        assert_eq!(
            quarantined.len(),
            1,
            "quarantined record must be preserved for inspection"
        );
        assert!(noted.exists(), "non-JSON files are logged, not deleted");
        // A second open must stay healthy now that the bad file is quarantined.
        open_session_recall(directory.path()).expect("reopen recall");
    }

    #[test]
    fn concurrent_openers_ingest_shared_inbox_without_missing_file_races() {
        let directory = tempfile::tempdir().expect("tempdir");
        let inbox = directory.path().join(".medusa/session-recall-inbox");
        fs::create_dir_all(&inbox).expect("inbox");
        for index in 0..8 {
            fs::write(
                inbox.join(format!("session-{index}.json")),
                serde_json::to_vec_pretty(&record(&format!("session-{index}"))).expect("serialize"),
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
