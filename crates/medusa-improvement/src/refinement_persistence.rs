//! Small, cross-platform persistence primitives for the canonical refinement authority.

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, SystemTime},
};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const LOCK_ATTEMPTS: usize = 8;
const LOCK_RETRY: Duration = Duration::from_millis(25);
const STALE_LOCK_AGE: Duration = Duration::from_secs(300);

pub(crate) struct AuthorityLock {
    path: PathBuf,
}

pub(crate) fn acquire_lock(root: &Path) -> io::Result<AuthorityLock> {
    fs::create_dir_all(root)?;
    let path = root.join("authority.lock");
    for _ in 0..LOCK_ATTEMPTS {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                if let Err(error) =
                    writeln!(file, "pid={}", std::process::id()).and_then(|()| file.sync_all())
                {
                    let _ = fs::remove_file(&path);
                    return Err(error);
                }
                return Ok(AuthorityLock { path });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if is_stale(&path) {
                    let _ = fs::remove_file(&path);
                    continue;
                }
                thread::sleep(LOCK_RETRY);
            }
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        format!("canonical refinement authority is busy: {}", path.display()),
    ))
}

impl Drop for AuthorityLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn is_stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age > STALE_LOCK_AGE)
}

pub(crate) fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) use medusa_core::storage::atomic_write;

pub(crate) fn quarantine_bytes(root: &Path, label: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    let directory = root.join("quarantine");
    fs::create_dir_all(&directory)?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = directory.join(format!("{label}-{}-{sequence}.bin", current_unix_ms()));
    atomic_write(&path, bytes)?;
    Ok(path)
}

pub(crate) fn remove_file_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub(crate) fn current_unix_ms() -> i64 {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}
