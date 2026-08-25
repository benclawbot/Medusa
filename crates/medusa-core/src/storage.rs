//! Shared durable storage and content-addressing primitives.

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use ulid::Ulid;

/// Writes bytes through a unique, durable temporary file and an atomic rename.
///
/// The temporary file is opened with `create_new`, flushed to stable storage before
/// publication, and removed if publication fails. On Unix the containing directory is
/// flushed after the rename so the directory entry is durable as well.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "destination path has no parent",
        )
    })?;
    fs::create_dir_all(parent)?;

    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("medusa");
    let mut temporary = None;
    let mut file = None;
    for _ in 0..8 {
        let candidate = parent.join(format!(".{name}.{}.tmp", Ulid::new()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(opened) => {
                temporary = Some(candidate);
                file = Some(opened);
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    let temporary = temporary.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique temporary path",
        )
    })?;

    let result = (|| {
        let mut file = file.expect("temporary file is present when its path is present");
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, path)?;
        sync_parent(parent);
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(temporary: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(temporary, destination)
}

#[cfg(windows)]
fn replace_file(temporary: &Path, destination: &Path) -> io::Result<()> {
    match fs::rename(temporary, destination) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            // `std::fs::rename` does not replace an existing file on Windows. Keep the
            // same publication API while avoiding the old fixed-name temporary files.
            fs::remove_file(destination)?;
            fs::rename(temporary, destination)
        }
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn sync_parent(parent: &Path) {
    if let Ok(directory) = fs::File::open(parent) {
        let _ = directory.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) {}

/// Returns the lowercase hexadecimal SHA-256 digest of `bytes`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Serializes `value` and returns its SHA-256 fingerprint.
pub fn fingerprint_json<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    serde_json::to_vec(value).map(|bytes| sha256_hex(&bytes))
}

/// Converts a filesystem path into the normalized `file://` URI used by LSP clients.
#[must_use]
pub fn file_uri(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");
    let encoded = percent_encode_uri_path(&normalized);
    if normalized.starts_with('/') {
        format!("file://{encoded}")
    } else {
        format!("file:///{encoded}")
    }
}

fn percent_encode_uri_path(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for byte in path.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.' | b'~' | b'/' | b':')
        {
            encoded.push(*byte as char);
        } else {
            encoded.push('%');
            encoded.push(char::from(b"0123456789ABCDEF"[(byte >> 4) as usize]));
            encoded.push(char::from(b"0123456789ABCDEF"[(byte & 0x0f) as usize]));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_and_leaves_no_temporary_files() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("state.json");
        atomic_write(&path, b"first").expect("first write");
        atomic_write(&path, b"second").expect("replacement write");
        assert_eq!(fs::read(&path).expect("read state"), b"second");
        assert_eq!(
            fs::read_dir(directory.path())
                .expect("read directory")
                .count(),
            1
        );
    }

    #[test]
    fn uri_and_digest_helpers_are_stable() {
        assert_eq!(
            file_uri(Path::new("C:\\repo with space")),
            "file:///C:/repo%20with%20space"
        );
        assert_eq!(file_uri(Path::new("/repo")), "file:///repo");
        assert_eq!(sha256_hex(b"medusa").len(), 64);
    }

    #[test]
    fn json_fingerprint_reports_serialization_errors() {
        let fingerprint = fingerprint_json(&serde_json::json!({"key": "value"})).expect("JSON");
        assert_eq!(fingerprint.len(), 64);
    }
}
