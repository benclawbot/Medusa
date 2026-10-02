use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Writes bytes to a uniquely named temporary file, flushes them, and publishes the file by
/// replacing `destination` through the platform replacement primitive.
pub fn atomic_write(destination: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = destination.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "destination path has no parent")
    })?;
    fs::create_dir_all(parent)?;
    let name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("medusa");
    let epoch_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    let mut temporary = None;
    let mut file = None;
    for _ in 0..8 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{name}.{}.{}.{}.tmp",
            std::process::id(),
            epoch_nanos,
            sequence
        ));
        match OpenOptions::new()
            .create_new(true)
            .write(true)
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
        let mut file =
            file.ok_or_else(|| io::Error::other("temporary file was not opened"))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, destination)?;
        sync_parent(parent);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Moves a prepared file into `destination`, replacing an existing file when supported.
///
/// Callers must close writable handles to both paths before calling this function.
pub fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    replace_file_impl(source, destination)
}

#[cfg(windows)]
fn replace_file_impl(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();

    // SAFETY: both UTF-16 buffers are NUL-terminated and live for the duration of the call.
    // MoveFileExW does not retain either pointer after returning.
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(windows))]
fn replace_file_impl(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(unix)]
fn sync_parent(parent: &Path) {
    if let Ok(directory) = fs::File::open(parent) {
        let _ = directory.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_existing_destination_and_consumes_source() {
        let directory = tempfile::tempdir().expect("tempdir");
        let source = directory.path().join("source.tmp");
        let destination = directory.path().join("destination.json");
        fs::write(&destination, b"old").expect("old destination");
        fs::write(&source, b"new").expect("source");

        replace_file(&source, &destination).expect("replace");

        assert_eq!(fs::read(&destination).expect("destination"), b"new");
        assert!(!source.exists());
    }

    #[test]
    fn atomic_write_replaces_existing_destination_without_temp_leaks() {
        let directory = tempfile::tempdir().expect("tempdir");
        let destination = directory.path().join("state.json");
        atomic_write(&destination, b"first").expect("first");
        atomic_write(&destination, b"second").expect("second");
        assert_eq!(fs::read(&destination).expect("destination"), b"second");
        assert_eq!(
            fs::read_dir(directory.path())
                .expect("directory")
                .count(),
            1
        );
    }
}
