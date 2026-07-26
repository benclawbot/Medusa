use medusa_memory::{SessionEvent, SessionRecallStore, SessionRecord, SessionSearchQuery};
use tempfile::tempdir;
use time::OffsetDateTime;

#[test]
fn shipped_runtime_can_persist_and_recall_prior_sessions() {
    let directory = tempdir().expect("tempdir");
    let database = directory.path().join("session-recall.sqlite3");
    let store = SessionRecallStore::open(&database).expect("open recall store");
    let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("timestamp");

    store
        .record_session(&SessionRecord {
            session_id: "session-previous".to_owned(),
            project_id: "project-medusa".to_owned(),
            started_at: now,
            ended_at: Some(now),
            summary: "Validated the workspace architecture graph".to_owned(),
            events: vec![SessionEvent {
                sequence: 0,
                kind: "verification".to_owned(),
                summary: "All shipped crates are reachable".to_owned(),
                observed_at: now,
            }],
        })
        .expect("record prior session");

    let results = store
        .search(&SessionSearchQuery {
            project_id: Some("project-medusa".to_owned()),
            text: "architecture graph".to_owned(),
            limit: 5,
        })
        .expect("search prior sessions");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].session.session_id, "session-previous");
}
