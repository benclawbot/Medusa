use std::{
    fs::{File, OpenOptions},
    io,
    path::Path,
};

/// An operating-system released exclusive lock over a persistent coordination file.
///
/// The file itself intentionally remains on disk. The kernel lock, rather than a marker file's
/// age or a possibly reused PID, is the ownership authority, so an abrupt process exit releases
/// it without requiring recovery heuristics.
#[derive(Debug)]
pub struct ExclusiveFileLock {
    file: File,
}

impl ExclusiveFileLock {
    /// Opens or creates `path` and attempts a non-blocking exclusive lock.
    pub fn try_acquire(path: &Path) -> io::Result<Self> {
        let file = open_lock_file(path)?;
        try_lock_exclusive(&file)?;
        Ok(Self { file })
    }
}

impl Drop for ExclusiveFileLock {
    fn drop(&mut self) {
        let _ = unlock_exclusive(&self.file);
    }
}

#[cfg(unix)]
fn open_lock_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(windows)]
fn open_lock_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_lock_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)
}

#[cfg(unix)]
fn try_lock_exclusive(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    // SAFETY: `file` owns a valid descriptor for the duration of this call and flock does not
    // dereference any caller-provided pointer.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn unlock_exclusive(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    // SAFETY: `file` owns a valid descriptor for the duration of this call and flock does not
    // dereference any caller-provided pointer.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn try_lock_exclusive(file: &File) -> io::Result<()> {
    use std::{mem::zeroed, os::windows::io::AsRawHandle};
    use windows_sys::Win32::{
        Foundation::{ERROR_LOCK_VIOLATION, HANDLE},
        Storage::FileSystem::{LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx},
        System::IO::OVERLAPPED,
    };

    let mut overlapped: OVERLAPPED = unsafe { zeroed() };
    // SAFETY: the file handle is owned by `file`, `overlapped` is valid writable storage, and the
    // requested one-byte range is entirely within the file-lock API's documented contract.
    let result = unsafe {
        LockFileEx(
            file.as_raw_handle() as HANDLE,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut overlapped,
        )
    };
    if result != 0 {
        Ok(())
    } else {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
            Err(io::Error::new(io::ErrorKind::WouldBlock, error))
        } else {
            Err(error)
        }
    }
}

#[cfg(windows)]
fn unlock_exclusive(file: &File) -> io::Result<()> {
    use std::{mem::zeroed, os::windows::io::AsRawHandle};
    use windows_sys::Win32::{
        Foundation::HANDLE, Storage::FileSystem::UnlockFileEx, System::IO::OVERLAPPED,
    };

    let mut overlapped: OVERLAPPED = unsafe { zeroed() };
    // SAFETY: the file handle and OVERLAPPED range match the successful lock acquired above.
    let result = unsafe { UnlockFileEx(file.as_raw_handle() as HANDLE, 0, 1, 0, &mut overlapped) };
    if result != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(unix, windows)))]
fn try_lock_exclusive(_file: &File) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "exclusive file locks are unsupported on this platform",
    ))
}

#[cfg(not(any(unix, windows)))]
fn unlock_exclusive(_file: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_owner_releases_kernel_lock_without_removing_lock_path() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("continuity.lock");
        let owner = ExclusiveFileLock::try_acquire(&path).expect("owner lock");
        assert!(ExclusiveFileLock::try_acquire(&path).is_err());
        drop(owner);
        assert!(path.exists());
        let replacement = ExclusiveFileLock::try_acquire(&path).expect("replacement lock");
        drop(replacement);
    }
}
