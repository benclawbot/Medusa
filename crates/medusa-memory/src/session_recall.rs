use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::support::sql_error;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionEvent {
    pub ordinal: usize,
    pub kind: String,
    pub tool: Option<String>,
    pub success: Option<bool>,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionRecord {
    pub session_id: String,
    pub parent_session_id: Option<String>,
    pub created_at: String,
    pub repository_fingerprint: String,
    pub outcome: String,
    pub events: Vec<SessionEvent>,
}

impl SessionRecord {
    fn validate(&self) -> MedusaResult<()> {
        if self.session_id.trim().is_empty()
            || self.created_at.trim().is_empty()
            || self.repository_fingerprint.trim().is_empty()
            || self.outcome.trim().is_empty()
            || self.events.is_empty()
        {
            return Err(invalid("session recall record is incomplete"));
        }
        Ok(())
    }

    fn tools(&self) -> BTreeSet<String> {
        self.events
            .iter()
            .filter_map(|event| event.tool.clone())
            .collect()
    }

    fn searchable_text(&self) -> String {
        self.events
            .iter()
            .map(|event| event.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionSearchQuery {
    pub query: String,
    pub repository_fingerprint: Option<String>,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    pub tool: Option<String>,
    pub outcome: Option<String>,
    pub limit: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionSearchHit {
    pub session_id: String,
    pub parent_session_id: Option<String>,
    pub created_at: String,
    pub repository_fingerprint: String,
    pub outcome: String,
    pub excerpt: String,
    pub relevance_milli: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionWindow {
    pub session_id: String,
    pub parent_session_id: Option<String>,
    pub events: Vec<SessionEvent>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionComparison {
    pub session_a: String,
    pub session_b: String,
    pub same_repository: bool,
    pub same_outcome: bool,
    pub shared_tools: BTreeSet<String>,
    pub only_a_tools: BTreeSet<String>,
    pub only_b_tools: BTreeSet<String>,
    pub successful_events_a: usize,
    pub successful_events_b: usize,
    pub failed_events_a: usize,
    pub failed_events_b: usize,
}

pub struct SessionRecallStore {
    path: PathBuf,
}

impl SessionRecallStore {
    pub fn new(root: impl AsRef<Path>) -> MedusaResult<Self> {
        let directory = root.as_ref().join(".medusa");
        std::fs::create_dir_all(&directory)?;
        let store = Self {
            path: directory.join("session-recall.sqlite3"),
        };
        store.initialize()?;
        Ok(store)
    }

    pub fn upsert(&self, record: &SessionRecord) -> MedusaResult<()> {
        record.validate()?;
        let connection = self.connection()?;
        let tools_json = serde_json::to_string(&record.tools())?;
        let events_json = serde_json::to_string(&record.events)?;
        connection
            .execute(
                "DELETE FROM session_recall WHERE session_id = ?1",
                params![record.session_id],
            )
            .map_err(sql_error)?;
        connection
            .execute(
                "INSERT INTO session_recall
                 (session_id, parent_session_id, created_at, repository_fingerprint,
                  tools_json, outcome, events_json, text)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    record.session_id,
                    record.parent_session_id,
                    record.created_at,
                    record.repository_fingerprint,
                    tools_json,
                    record.outcome,
                    events_json,
                    record.searchable_text(),
                ],
            )
            .map_err(sql_error)?;
        Ok(())
    }

    pub fn session_search(
        &self,
        query: &SessionSearchQuery,
    ) -> MedusaResult<Vec<SessionSearchHit>> {
        let expression = fts_expression(&query.query)?;
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT session_id, parent_session_id, created_at, repository_fingerprint,
                        tools_json, outcome,
                        snippet(session_recall, 7, '', '', ' … ', 28),
                        bm25(session_recall)
                 FROM session_recall
                 WHERE session_recall MATCH ?1
                 ORDER BY bm25(session_recall), created_at DESC",
            )
            .map_err(sql_error)?;
        let rows = statement
            .query_map(params![expression], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, f64>(7)?,
                ))
            })
            .map_err(sql_error)?;

        let limit = query.limit.clamp(1, 100);
        let mut hits = Vec::new();
        for row in rows {
            let (
                session_id,
                parent_session_id,
                created_at,
                repository,
                tools_json,
                outcome,
                excerpt,
                rank,
            ) = row.map_err(sql_error)?;
            let tools: BTreeSet<String> = serde_json::from_str(&tools_json)?;
            if query
                .repository_fingerprint
                .as_ref()
                .is_some_and(|value| value != &repository)
                || query
                    .tool
                    .as_ref()
                    .is_some_and(|value| !tools.contains(value))
                || query
                    .outcome
                    .as_ref()
                    .is_some_and(|value| value != &outcome)
                || query
                    .date_from
                    .as_ref()
                    .is_some_and(|value| &created_at < value)
                || query
                    .date_to
                    .as_ref()
                    .is_some_and(|value| &created_at > value)
            {
                continue;
            }
            hits.push(SessionSearchHit {
                session_id,
                parent_session_id,
                created_at,
                repository_fingerprint: repository,
                outcome,
                excerpt,
                relevance_milli: (-rank * 1_000.0).round() as i64,
            });
            if hits.len() == limit {
                break;
            }
        }
        Ok(hits)
    }

    pub fn session_open(
        &self,
        session_id: &str,
        around_event: Option<usize>,
        radius: usize,
    ) -> MedusaResult<SessionWindow> {
        let record = self.read(session_id)?;
        let events = match around_event {
            Some(ordinal) => record
                .events
                .into_iter()
                .filter(|event| event.ordinal.abs_diff(ordinal) <= radius)
                .collect(),
            None => record.events,
        };
        Ok(SessionWindow {
            session_id: record.session_id,
            parent_session_id: record.parent_session_id,
            events,
        })
    }

    pub fn session_compare(
        &self,
        session_a: &str,
        session_b: &str,
    ) -> MedusaResult<SessionComparison> {
        let a = self.read(session_a)?;
        let b = self.read(session_b)?;
        let tools_a = a.tools();
        let tools_b = b.tools();
        Ok(SessionComparison {
            session_a: a.session_id,
            session_b: b.session_id,
            same_repository: a.repository_fingerprint == b.repository_fingerprint,
            same_outcome: a.outcome == b.outcome,
            shared_tools: tools_a.intersection(&tools_b).cloned().collect(),
            only_a_tools: tools_a.difference(&tools_b).cloned().collect(),
            only_b_tools: tools_b.difference(&tools_a).cloned().collect(),
            successful_events_a: count_events(&a.events, true),
            successful_events_b: count_events(&b.events, true),
            failed_events_a: count_events(&a.events, false),
            failed_events_b: count_events(&b.events, false),
        })
    }

    fn read(&self, session_id: &str) -> MedusaResult<SessionRecord> {
        let connection = self.connection()?;
        let row = connection
            .query_row(
                "SELECT session_id, parent_session_id, created_at, repository_fingerprint,
                        outcome, events_json
                 FROM session_recall WHERE session_id = ?1",
                params![session_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(sql_error)?
            .ok_or_else(|| invalid(format!("unknown session {session_id}")))?;
        Ok(SessionRecord {
            session_id: row.0,
            parent_session_id: row.1,
            created_at: row.2,
            repository_fingerprint: row.3,
            outcome: row.4,
            events: serde_json::from_str(&row.5)?,
        })
    }

    fn initialize(&self) -> MedusaResult<()> {
        self.connection()?
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE VIRTUAL TABLE IF NOT EXISTS session_recall USING fts5(
                   session_id UNINDEXED,
                   parent_session_id UNINDEXED,
                   created_at UNINDEXED,
                   repository_fingerprint UNINDEXED,
                   tools_json UNINDEXED,
                   outcome UNINDEXED,
                   events_json UNINDEXED,
                   text
                 );",
            )
            .map_err(sql_error)
    }

    fn connection(&self) -> MedusaResult<Connection> {
        Connection::open(&self.path).map_err(sql_error)
    }
}

fn count_events(events: &[SessionEvent], success: bool) -> usize {
    events
        .iter()
        .filter(|event| event.success == Some(success))
        .count()
}

fn fts_expression(query: &str) -> MedusaResult<String> {
    let terms = query
        .split_whitespace()
        .map(|term| term.trim_matches(|character: char| !character.is_alphanumeric()))
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return Err(invalid(
            "session search requires at least one searchable term",
        ));
    }
    Ok(terms.join(" AND "))
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::InvalidInput, ErrorCategory::Validation, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, parent: Option<&str>, outcome: &str) -> SessionRecord {
        SessionRecord {
            session_id: id.to_owned(),
            parent_session_id: parent.map(str::to_owned),
            created_at: "2026-07-20T20:00:00Z".to_owned(),
            repository_fingerprint: "sha256:repo".to_owned(),
            outcome: outcome.to_owned(),
            events: vec![
                SessionEvent {
                    ordinal: 0,
                    kind: "user".to_owned(),
                    tool: None,
                    success: None,
                    text: "Fix Windows Cargo executable replacement".to_owned(),
                },
                SessionEvent {
                    ordinal: 1,
                    kind: "tool".to_owned(),
                    tool: Some("shell".to_owned()),
                    success: Some(true),
                    text: "Used a detached helper to replace the executable".to_owned(),
                },
            ],
        }
    }

    #[test]
    fn fts_search_preserves_lineage_and_filters() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = SessionRecallStore::new(directory.path()).expect("store");
        store
            .upsert(&record("child", Some("parent"), "success"))
            .expect("upsert");
        let hits = store
            .session_search(&SessionSearchQuery {
                query: "Windows Cargo replacement".to_owned(),
                repository_fingerprint: Some("sha256:repo".to_owned()),
                tool: Some("shell".to_owned()),
                outcome: Some("success".to_owned()),
                limit: 10,
                ..SessionSearchQuery::default()
            })
            .expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].parent_session_id.as_deref(), Some("parent"));
    }

    #[test]
    fn open_can_return_a_window_around_an_event() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = SessionRecallStore::new(directory.path()).expect("store");
        store
            .upsert(&record("session", None, "success"))
            .expect("upsert");
        let window = store.session_open("session", Some(1), 0).expect("open");
        assert_eq!(window.events.len(), 1);
        assert_eq!(window.events[0].ordinal, 1);
    }

    #[test]
    fn compare_reports_shared_tools_and_outcomes() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = SessionRecallStore::new(directory.path()).expect("store");
        store.upsert(&record("a", None, "success")).expect("a");
        store.upsert(&record("b", None, "failure")).expect("b");
        let comparison = store.session_compare("a", "b").expect("compare");
        assert!(comparison.shared_tools.contains("shell"));
        assert!(!comparison.same_outcome);
    }
}
