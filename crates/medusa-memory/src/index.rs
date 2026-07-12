use std::fs;

use medusa_core::MedusaResult;
use rusqlite::{params, Connection};

use crate::{engine::MemoryEngine, support::sql_error};

impl MemoryEngine {
    /// Rebuilds the complete machine index exclusively from canonical Markdown.
    pub fn rebuild_index(&self) -> MedusaResult<()> {
        if self.index_path.exists() {
            fs::remove_file(&self.index_path)?;
        }
        let connection = Connection::open(&self.index_path).map_err(sql_error)?;
        create_schema(&connection)?;
        for (path, document) in self.documents()? {
            connection
                .execute(
                    "INSERT INTO memory_documents
                     (id, path, type, title, body, scope, status, confidence_milli, validation,
                      updated_at, expires_at, successful_reuse_count)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        document.id,
                        path.to_string_lossy(),
                        document.memory_type,
                        document.title,
                        document.body,
                        document.scope.as_str(),
                        document.status.as_str(),
                        document.confidence_milli,
                        document.validation.as_str(),
                        document.updated_at,
                        document.expires_at,
                        document.successful_reuse_count,
                    ],
                )
                .map_err(sql_error)?;
            for tag in &document.tags {
                connection
                    .execute(
                        "INSERT INTO memory_tags (document_id, tag) VALUES (?1, ?2)",
                        params![document.id, tag],
                    )
                    .map_err(sql_error)?;
            }
            for source in &document.sources {
                connection
                    .execute(
                        "INSERT INTO memory_validation (document_id, source) VALUES (?1, ?2)",
                        params![document.id, source],
                    )
                    .map_err(sql_error)?;
            }
            for target in &document.supersedes {
                connection
                    .execute(
                        "INSERT INTO memory_links (source_id, target_id, relation)
                         VALUES (?1, ?2, 'supersedes')",
                        params![document.id, target],
                    )
                    .map_err(sql_error)?;
            }
        }
        Ok(())
    }
}

fn create_schema(connection: &Connection) -> MedusaResult<()> {
    connection
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE memory_documents (
               id TEXT PRIMARY KEY,
               path TEXT NOT NULL,
               type TEXT NOT NULL,
               title TEXT NOT NULL,
               body TEXT NOT NULL,
               scope TEXT NOT NULL,
               status TEXT NOT NULL,
               confidence_milli INTEGER NOT NULL,
               validation TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               expires_at TEXT,
               successful_reuse_count INTEGER NOT NULL
             );
             CREATE TABLE memory_chunks (
               document_id TEXT NOT NULL,
               ordinal INTEGER NOT NULL,
               content TEXT NOT NULL,
               PRIMARY KEY (document_id, ordinal)
             );
             CREATE TABLE memory_links (
               source_id TEXT NOT NULL,
               target_id TEXT NOT NULL,
               relation TEXT NOT NULL
             );
             CREATE TABLE memory_tags (
               document_id TEXT NOT NULL,
               tag TEXT NOT NULL
             );
             CREATE TABLE memory_validation (
               document_id TEXT NOT NULL,
               source TEXT NOT NULL
             );",
        )
        .map_err(sql_error)
}
