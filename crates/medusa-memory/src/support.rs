use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use rusqlite::Connection;
use ulid::Ulid;

pub(crate) struct LifecycleLock {
    connection: Connection,
}

impl LifecycleLock {
    pub(crate) fn acquire(root: &Path) -> MedusaResult<Self> {
        fs::create_dir_all(root)?;
        let connection =
            Connection::open(root.join("lifecycle-lock.sqlite3")).map_err(sql_error)?;
        connection
            .busy_timeout(std::time::Duration::from_secs(30))
            .map_err(sql_error)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=DELETE;
                 CREATE TABLE IF NOT EXISTS lifecycle_lock (singleton INTEGER PRIMARY KEY CHECK (singleton = 1));
                 BEGIN IMMEDIATE;",
            )
            .map_err(sql_error)?;
        Ok(Self { connection })
    }
}

impl Drop for LifecycleLock {
    fn drop(&mut self) {
        let _ = self.connection.execute_batch("ROLLBACK;");
    }
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> MedusaResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("memory path must have a parent directory"))?;
    fs::create_dir_all(parent)?;

    let temporary = temporary_path(path);
    let result = (|| -> MedusaResult<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        sync_parent(parent)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn durable_remove(path: &Path) -> MedusaResult<()> {
    match fs::remove_file(path) {
        Ok(()) => {
            if let Some(parent) = path.parent() {
                sync_parent(parent)?;
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("memory");
    path.with_file_name(format!(".{file_name}.{}.tmp", Ulid::new()))
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> MedusaResult<()> {
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> MedusaResult<()> {
    Ok(())
}

pub(crate) fn tokenize(value: &str) -> Vec<String> {
    normalize(value)
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

pub(crate) fn normalize(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn sanitize_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect()
}

pub(crate) fn deduplicate(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub(crate) fn first_claim(body: &str) -> String {
    body.lines()
        .find(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .unwrap_or_default()
        .trim()
        .to_owned()
}

pub(crate) fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

pub(crate) fn internal(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        message,
    )
}

pub(crate) fn sql_error(error: rusqlite::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::PersistenceFailed,
        ErrorCategory::Persistence,
        error.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc, thread};

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn atomic_write_replaces_complete_contents() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("memory.md");
        fs::write(&path, b"old").expect("seed file");
        atomic_write(&path, b"new complete state").expect("atomic write");
        assert_eq!(
            fs::read(&path).expect("read final file"),
            b"new complete state"
        );
        assert_no_temporary_files(directory.path());
    }

    #[test]
    fn concurrent_writes_use_distinct_temporary_paths() {
        let directory = tempdir().expect("temporary directory");
        let first = directory.path().join("memory.md");
        let second = directory.path().join("memory.json");
        let first_worker = {
            let first = first.clone();
            thread::spawn(move || atomic_write(&first, b"markdown"))
        };
        let second_worker = {
            let second = second.clone();
            thread::spawn(move || atomic_write(&second, b"json"))
        };
        first_worker
            .join()
            .expect("first worker")
            .expect("first write");
        second_worker
            .join()
            .expect("second worker")
            .expect("second write");
        assert_eq!(fs::read(first).expect("read markdown"), b"markdown");
        assert_eq!(fs::read(second).expect("read json"), b"json");
        assert_no_temporary_files(directory.path());
    }

    #[test]
    fn lifecycle_lock_serializes_writers() {
        let directory = tempdir().expect("temporary directory");
        let root = Arc::new(directory.path().to_path_buf());
        let first = LifecycleLock::acquire(&root).expect("first lock");
        let second_root = Arc::clone(&root);
        let worker = thread::spawn(move || LifecycleLock::acquire(&second_root));
        thread::sleep(std::time::Duration::from_millis(50));
        assert!(!worker.is_finished());
        drop(first);
        worker.join().expect("worker").expect("second lock");
    }

    #[test]
    fn temporary_names_are_collision_resistant_and_same_directory() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("memory.md");
        let first = temporary_path(&path);
        let second = temporary_path(&path);
        assert_eq!(first.parent(), Some(directory.path()));
        assert_eq!(second.parent(), Some(directory.path()));
        assert_ne!(first, second);
        assert!(
            first
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".memory.md.")
        );
    }

    fn assert_no_temporary_files(directory: &Path) {
        let leftovers = fs::read_dir(directory)
            .expect("read directory")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect::<Vec<_>>();
        assert!(
            leftovers.is_empty(),
            "temporary files remain: {leftovers:?}"
        );
    }
}
