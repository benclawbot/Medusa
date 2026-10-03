//! Shared durable storage and content-addressing primitives.

use std::{
    fs,
    io::{self, Write},
    path::Path,
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tempfile::Builder;

/// Length of Windows `MAX_PATH`, which still bounds Win32 path resolution when long-path
/// support is not enabled for the process.
const MAX_PATH: usize = 260;
/// Naming of the temporary file `atomic_write` publishes through.
const TEMPORARY_PREFIX: &str = ".medusa-write-";
const TEMPORARY_SUFFIX: &str = ".tmp";
/// Characters `tempfile` appends between prefix and suffix. Pinned here so the path-budget
/// assertion below describes the real name rather than `tempfile`'s default.
const TEMPORARY_RANDOM_CHARS: usize = 6;

/// Names the failing step and its target path.
///
/// A bare `io::Error` cannot distinguish which operation failed. On Windows both a missing
/// intermediate directory and an over-long path surface as
/// `The system cannot find the path specified. (os error 3)`.
fn at(step: &str, path: &Path, error: io::Error) -> io::Error {
    let rendered = path.display().to_string();
    let length = rendered.chars().count();
    let budget = if length > MAX_PATH {
        " exceeds-max-path"
    } else {
        ""
    };
    io::Error::new(
        error.kind(),
        format!("{step}: {error} [path={rendered} length={length}{budget}]"),
    )
}

/// Writes bytes through a unique, durable temporary file and an atomic rename.
///
/// The temporary file is opened with `create_new`, flushed to stable storage before
/// publication, and removed if publication fails. On Unix the containing directory is
/// flushed after the rename so the directory entry is durable as well.
///
/// The temporary file's name is deliberately short and does not echo the destination file
/// name. Content-addressed evidence paths are already long
/// (`<root>/objects/artifact-<64 hex>.bin`), and on Windows the destination sits close to
/// `MAX_PATH` (260). Repeating the 77-character destination name in the temporary file name
/// is what pushed the path over that limit, so every evidence write failed with
/// `The system cannot find the path specified. (os error 3)`.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "destination path has no parent",
        )
    })?;
    fs::create_dir_all(parent).map_err(|error| at("atomic-write:create-dir-all", parent, error))?;

    let mut temporary = Builder::new()
        .prefix(TEMPORARY_PREFIX)
        .suffix(TEMPORARY_SUFFIX)
        .rand_bytes(TEMPORARY_RANDOM_CHARS)
        .tempfile_in(parent)
        .map_err(|error| at("atomic-write:tempfile-in", parent, error))?;
    let temporary_path = temporary.path().to_path_buf();
    temporary
        .write_all(bytes)
        .map_err(|error| at("atomic-write:write-all", &temporary_path, error))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| at("atomic-write:sync-all", &temporary_path, error))?;
    temporary
        .persist(path)
        .map_err(|error| at("atomic-write:persist", path, error.error))?;
    sync_parent(parent);
    Ok(())
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

    /// Windows `MAX_PATH` is 260, and the temporary file lives in the destination's own
    /// parent directory, so its name consumes the budget left over by the destination.
    ///
    /// Content-addressed evidence objects are written as `artifact-<64 hex>.bin` under an
    /// already deep parent (the failing CI run used a 172-character parent, giving a
    /// 250-character destination). Echoing the 77-character destination name into the
    /// temporary name produced a 262-character path, and Windows rejected every evidence
    /// write with `The system cannot find the path specified. (os error 3)`.
    ///
    /// The rejection itself only reproduces on Windows, so the budget is asserted directly
    /// and a deep destination is written to exercise the real code path.
    #[test]
    fn atomic_write_keeps_its_temporary_path_within_windows_max_path() {
        let temporary_name =
            TEMPORARY_PREFIX.len() + TEMPORARY_RANDOM_CHARS + TEMPORARY_SUFFIX.len();
        // Parent depth of the evidence object path observed in the failing run.
        const EVIDENCE_OBJECT_PARENT: usize = 172;
        const EVIDENCE_OBJECT_NAME: usize = "artifact-".len() + 64 + ".bin".len();
        let destination = EVIDENCE_OBJECT_PARENT + 1 + EVIDENCE_OBJECT_NAME;
        assert!(
            destination <= MAX_PATH,
            "destination path {destination} exceeds MAX_PATH"
        );
        let temporary = EVIDENCE_OBJECT_PARENT + 1 + temporary_name;
        assert!(
            temporary <= MAX_PATH,
            "temporary path {temporary} exceeds MAX_PATH"
        );

        let directory = tempfile::tempdir().expect("temporary directory");
        let mut parent = directory.path().to_path_buf();
        while parent.as_os_str().len() < EVIDENCE_OBJECT_PARENT {
            let remaining = EVIDENCE_OBJECT_PARENT - parent.as_os_str().len() - 1;
            parent = parent.join("d".repeat(remaining.min(60)));
        }
        fs::create_dir_all(&parent).expect("deep parent");
        let name = format!("artifact-{}.bin", "f".repeat(64));
        let path = parent.join(name);
        assert_eq!(
            path.as_os_str().len(),
            EVIDENCE_OBJECT_PARENT + 1 + EVIDENCE_OBJECT_NAME
        );
        atomic_write(&path, b"evidence").expect("deep write");
        assert_eq!(fs::read(&path).expect("read object"), b"evidence");
        assert_eq!(
            fs::read_dir(&parent)
                .expect("read object directory")
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
