use medusa_memory::{SessionEvent, SessionRecallStore, SessionRecord, SessionSearchQuery};
use tempfile::tempdir;

#[test]
fn shipped_runtime_can_persist_and_recall_prior_sessions() {
    let directory = tempdir().expect("tempdir");
    let store = SessionRecallStore::new(directory.path()).expect("open recall store");

    store
        .upsert(&SessionRecord {
            session_id: "session-previous".to_owned(),
            parent_session_id: None,
            created_at: "2026-07-26T17:00:00Z".to_owned(),
            repository_fingerprint: "project-medusa".to_owned(),
            outcome: "verified".to_owned(),
            events: vec![SessionEvent {
                ordinal: 0,
                kind: "verification".to_owned(),
                tool: Some("cargo".to_owned()),
                success: Some(true),
                text: "Validated the workspace architecture graph".to_owned(),
            }],
        })
        .expect("record prior session");

    let results = store
        .session_search(&SessionSearchQuery {
            query: "architecture graph".to_owned(),
            repository_fingerprint: Some("project-medusa".to_owned()),
            date_from: None,
            date_to: None,
            tool: Some("cargo".to_owned()),
            outcome: Some("verified".to_owned()),
            limit: 5,
        })
        .expect("search prior sessions");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].session_id, "session-previous");
}
