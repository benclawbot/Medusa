use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use medusa_core::MedusaResult;
use rusqlite::{Connection, params};

use crate::{engine::MemoryEngine, support::sql_error};

impl MemoryEngine {
    /// Rebuilds the complete machine index exclusively from canonical Markdown.
    pub fn rebuild_index(&self) -> MedusaResult<()> {
        let documents = self.documents()?;
        let temporary_path = temporary_index_path(&self.index_path);
        let backup_path = backup_index_path(&self.index_path);
        let _ = fs::remove_file(&temporary_path);
        let _ = fs::remove_file(&backup_path);

        let mut connection = Connection::open(&temporary_path).map_err(sql_error)?;
        create_schema(&connection)?;
        let transaction = connection.transaction().map_err(sql_error)?;
        for (path, document) in documents {
            transaction
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
                transaction
                    .execute(
                        "INSERT INTO memory_tags (document_id, tag) VALUES (?1, ?2)",
                        params![document.id, tag],
                    )
                    .map_err(sql_error)?;
            }
            for source in &document.sources {
                transaction
                    .execute(
                        "INSERT INTO memory_validation (document_id, source) VALUES (?1, ?2)",
                        params![document.id, source],
                    )
                    .map_err(sql_error)?;
            }
            for target in &document.supersedes {
                transaction
                    .execute(
                        "INSERT INTO memory_links (source_id, target_id, relation)
                         VALUES (?1, ?2, 'supersedes')",
                        params![document.id, target],
                    )
                    .map_err(sql_error)?;
            }
        }
        transaction.commit().map_err(sql_error)?;
        // The published artifact is a single SQLite file. Checkpoint and leave WAL mode before
        // closing so a crash or rename cannot strand committed rows in a sidecar file.
        connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")
            .map_err(sql_error)?;
        connection.close().map_err(|(_, error)| sql_error(error))?;
        let _ = fs::remove_file(format!("{}-wal", temporary_path.display()));
        let _ = fs::remove_file(format!("{}-shm", temporary_path.display()));

        // Keep the prior complete index until the replacement is visible. A crash between the
        // two renames leaves the backup available for initialize_layout to restore.
        if self.index_path.exists() {
            fs::rename(&self.index_path, &backup_path)?;
        }
        if let Err(error) = fs::rename(&temporary_path, &self.index_path) {
            if backup_path.exists() && !self.index_path.exists() {
                let _ = fs::rename(&backup_path, &self.index_path);
            }
            let _ = fs::remove_file(&temporary_path);
            return Err(error.into());
        }
        let _ = fs::remove_file(&backup_path);
        Ok(())
    }
}

pub(crate) fn recover_index_swap(index_path: &Path) -> MedusaResult<()> {
    let backup_path = backup_index_path(index_path);
    if !index_path.exists() && backup_path.exists() {
        fs::rename(backup_path, index_path)?;
    }
    Ok(())
}

fn backup_index_path(index_path: &Path) -> PathBuf {
    index_path.with_extension("sqlite3.bak")
}

fn temporary_index_path(index_path: &Path) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let pid = std::process::id();
    index_path.with_extension(format!("sqlite3.tmp-{pid}-{timestamp}"))
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
