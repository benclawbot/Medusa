use std::{io, path::Path};

#[cfg(any(not(windows), test))]
use std::fs;

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
}
