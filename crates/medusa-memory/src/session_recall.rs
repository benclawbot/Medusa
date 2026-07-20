use std::{collections::BTreeSet, path::Path};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionQuery {
    pub text: String,
    pub repository: Option<String>,
    pub tool: Option<String>,
    pub outcome: Option<String>,
    pub limit: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionRecord {
    pub session_id: String,
    pub parent_session_id: Option<String>,
    pub repository: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub outcome: String,
    pub tools: BTreeSet<String>,
    pub transcript: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionHit {
    pub session_id: String,
    pub parent_session_id: Option<String>,
    pub repository: String,
    pub outcome: String,
    pub excerpt: String,
    pub rank_milli: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionComparison {
    pub left: SessionRecord,
    pub right: SessionRecord,
    pub shared_tools: BTreeSet<String>,
    pub only_left_tools: BTreeSet<String>,
    pub only_right_tools: BTreeSet<String>,
    pub same_repository: bool,
    pub same_outcome: bool,
}

pub struct SessionRecallStore {
    connection: Connection,
}

impl SessionRecallStore {
    pub fn open(path: impl AsRef<Path>) -> MedusaResult<Self> {
        let connection = Connection::open(path).map_err(database_error)?;
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE IF NOT EXISTS sessions (
                   session_id TEXT PRIMARY KEY,
                   parent_session_id TEXT,
                   repository TEXT NOT NULL,
                   started_at TEXT NOT NULL,
                   ended_at TEXT,
                   outcome TEXT NOT NULL,
                   tools_json TEXT NOT NULL,
                   transcript TEXT NOT NULL
                 );
                 CREATE VIRTUAL TABLE IF NOT EXISTS sessions_fts USING fts5(
                   session_id UNINDEXED,
                   repository,
                   outcome,
                   tools,
                   transcript,
                   tokenize='unicode61'
                 );",
            )
            .map_err(database_error)?;
        Ok(Self { connection })
    }

    pub fn upsert(&mut self, record: &SessionRecord) -> MedusaResult<()> {
        validate_record(record)?;
        let tools_json = serde_json::to_string(&record.tools)?;
        let tools_text = record.tools.iter().cloned().collect::<Vec<_>>().join(" ");
        let transaction = self.connection.transaction().map_err(database_error)?;
        transaction
            .execute(
                "INSERT INTO sessions(session_id,parent_session_id,repository,started_at,ended_at,outcome,tools_json,transcript)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(session_id) DO UPDATE SET
                   parent_session_id=excluded.parent_session_id,
                   repository=excluded.repository,
                   started_at=excluded.started_at,
                   ended_at=excluded.ended_at,
                   outcome=excluded.outcome,
                   tools_json=excluded.tools_json,
                   transcript=excluded.transcript",
                params![record.session_id, record.parent_session_id, record.repository, record.started_at, record.ended_at, record.outcome, tools_json, record.transcript],
            )
            .map_err(database_error)?;
        transaction
            .execute("DELETE FROM sessions_fts WHERE session_id = ?1", [&record.session_id])
            .map_err(database_error)?;
        transaction
            .execute(
                "INSERT INTO sessions_fts(session_id,repository,outcome,tools,transcript) VALUES (?1,?2,?3,?4,?5)",
                params![record.session_id, record.repository, record.outcome, tools_text, record.transcript],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }

    pub fn search(&self, query: &SessionQuery) -> MedusaResult<Vec<SessionHit>> {
        if query.text.trim().is_empty() {
            return Err(invalid("session search text cannot be empty"));
        }
        let limit = query.limit.clamp(1, 100) as i64;
        let mut statement = self.connection.prepare(
            "SELECT s.session_id,s.parent_session_id,s.repository,s.outcome,
                    snippet(sessions_fts,4,'[',']',' … ',24),
                    CAST((-bm25(sessions_fts))*1000 AS INTEGER)
             FROM sessions_fts
             JOIN sessions s ON s.session_id = sessions_fts.session_id
             WHERE sessions_fts MATCH ?1
               AND (?2 IS NULL OR s.repository = ?2)
               AND (?3 IS NULL OR sessions_fts.tools MATCH ?3)
               AND (?4 IS NULL OR s.outcome = ?4)
             ORDER BY bm25(sessions_fts), s.started_at DESC
             LIMIT ?5",
        ).map_err(database_error)?;
        let rows = statement.query_map(
            params![query.text, query.repository, query.tool, query.outcome, limit],
            |row| Ok(SessionHit {
                session_id: row.get(0)?,
                parent_session_id: row.get(1)?,
                repository: row.get(2)?,
                outcome: row.get(3)?,
                excerpt: row.get(4)?,
                rank_milli: row.get(5)?,
            }),
        ).map_err(database_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(database_error)
    }

    pub fn open_session(&self, session_id: &str) -> MedusaResult<Option<SessionRecord>> {
        self.connection.query_row(
            "SELECT session_id,parent_session_id,repository,started_at,ended_at,outcome,tools_json,transcript FROM sessions WHERE session_id=?1",
            [session_id],
            decode_record,
        ).optional().map_err(database_error)
    }

    pub fn lineage(&self, session_id: &str) -> MedusaResult<Vec<SessionRecord>> {
        let mut result = Vec::new();
        let mut current = self.open_session(session_id)?;
        while let Some(record) = current {
            let parent = record.parent_session_id.clone();
            result.push(record);
            current = match parent { Some(id) => self.open_session(&id)?, None => None };
        }
        Ok(result)
    }

    pub fn compare(&self, left: &str, right: &str) -> MedusaResult<SessionComparison> {
        let left = self.open_session(left)?.ok_or_else(|| invalid("left session not found"))?;
        let right = self.open_session(right)?.ok_or_else(|| invalid("right session not found"))?;
        let shared_tools = left.tools.intersection(&right.tools).cloned().collect();
        let only_left_tools = left.tools.difference(&right.tools).cloned().collect();
        let only_right_tools = right.tools.difference(&left.tools).cloned().collect();
        Ok(SessionComparison {
            same_repository: left.repository == right.repository,
            same_outcome: left.outcome == right.outcome,
            left,
            right,
            shared_tools,
            only_left_tools,
            only_right_tools,
        })
    }
}

fn decode_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRecord> {
    let tools_json: String = row.get(6)?;
    let tools = serde_json::from_str(&tools_json).map_err(|error| rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(error)))?;
    Ok(SessionRecord {
        session_id: row.get(0)?,
        parent_session_id: row.get(1)?,
        repository: row.get(2)?,
        started_at: row.get(3)?,
        ended_at: row.get(4)?,
        outcome: row.get(5)?,
        tools,
        transcript: row.get(7)?,
    })
}

fn validate_record(record: &SessionRecord) -> MedusaResult<()> {
    if record.session_id.trim().is_empty() || record.repository.trim().is_empty() || record.started_at.trim().is_empty() || record.transcript.trim().is_empty() {
        return Err(invalid("session record is incomplete"));
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::InvalidInput, ErrorCategory::Validation, message)
}

fn database_error(error: rusqlite::Error) -> MedusaError {
    MedusaError::new(ErrorCode::PersistenceFailed, ErrorCategory::Persistence, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, parent: Option<&str>, text: &str) -> SessionRecord {
        SessionRecord {
            session_id: id.to_owned(),
            parent_session_id: parent.map(str::to_owned),
            repository: "repo-a".to_owned(),
            started_at: "2026-07-20T00:00:00Z".to_owned(),
            ended_at: Some("2026-07-20T00:10:00Z".to_owned()),
            outcome: "success".to_owned(),
            tools: BTreeSet::from(["shell".to_owned(), "patch".to_owned()]),
            transcript: text.to_owned(),
        }
    }

    #[test]
    fn searches_opens_compares_and_tracks_lineage() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut store = SessionRecallStore::open(directory.path().join("sessions.sqlite")).expect("store");
        store.upsert(&record("parent", None, "fixed Windows Cargo executable replacement using detached helper")).expect("parent");
        store.upsert(&record("child", Some("parent"), "verified replacement and release gates")).expect("child");
        let hits = store.search(&SessionQuery { text: "Windows Cargo replacement".to_owned(), repository: Some("repo-a".to_owned()), tool: Some("shell".to_owned()), outcome: Some("success".to_owned()), limit: 10 }).expect("search");
        assert_eq!(hits[0].session_id, "parent");
        assert_eq!(store.lineage("child").expect("lineage").len(), 2);
        let comparison = store.compare("parent", "child").expect("compare");
        assert!(comparison.same_repository);
        assert!(comparison.shared_tools.contains("shell"));
    }
}
