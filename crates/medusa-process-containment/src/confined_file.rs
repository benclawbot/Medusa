use std::{
    fs::File,
    io::{self, Read},
    path::{Component, Path},
};

#[cfg(unix)]
use std::{
    ffi::{CString, OsStr},
    mem::MaybeUninit,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    },
};
#[cfg(windows)]
use std::{
    fs::OpenOptions,
    os::windows::fs::{MetadataExt, OpenOptionsExt},
    path::PathBuf,
};

#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_SHARE_READ, FILE_SHARE_WRITE,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfinedReadError {
    Invalid,
    Symlink,
    Missing,
    Io,
}

#[derive(Debug)]
pub struct ConfinedDir {
    _root: File,
    #[cfg(windows)]
    root_path: PathBuf,
}

impl ConfinedDir {
    pub fn open(root: &Path) -> Result<Self, ConfinedReadError> {
        #[cfg(unix)]
        {
            let root = open_unix_path(
                root,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )?;
            if !root.metadata().map_err(map_io_error)?.is_dir() {
                return Err(ConfinedReadError::Invalid);
            }
            Ok(Self { _root: root })
        }
        #[cfg(windows)]
        {
            let root_path = root.to_path_buf();
            let root = open_windows_path(&root_path, true)?;
            Ok(Self {
                _root: root,
                root_path,
            })
        }
    }

    pub fn read(&self, relative: &Path) -> Result<Vec<u8>, ConfinedReadError> {
        validate_relative(relative)?;
        #[cfg(unix)]
        {
            self.read_unix(relative)
        }
        #[cfg(windows)]
        {
            self.read_windows(relative)
        }
    }

    #[cfg(unix)]
    fn read_unix(&self, relative: &Path) -> Result<Vec<u8>, ConfinedReadError> {
        let mut current = self._root.try_clone().map_err(map_io_error)?;
        let mut components = relative.components().peekable();
        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                return Err(ConfinedReadError::Invalid);
            };
            let is_last = components.peek().is_none();
            let flags = if is_last {
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW
            } else {
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW
            };
            let next = open_unix_at(&current, name, flags)?;
            if is_last {
                if !next.metadata().map_err(map_io_error)?.is_file() {
                    return Err(ConfinedReadError::Invalid);
                }
                return read_file(next);
            }
            current = next;
        }
        Err(ConfinedReadError::Invalid)
    }

    #[cfg(windows)]
    fn read_windows(&self, relative: &Path) -> Result<Vec<u8>, ConfinedReadError> {
        // `_root` is intentionally retained even though path traversal below uses root_path:
        // keeping the handle alive without FILE_SHARE_DELETE pins the authorized root for the
        // lifetime of this capability.
        // Each intermediate directory is opened the same way and retained until the final file
        // has been opened, so no validated ancestor can be renamed or swapped while path
        // resolution proceeds. FILE_FLAG_OPEN_REPARSE_POINT ensures each opened component is
        // inspected rather than followed.
        let mut pinned_directories = Vec::new();
        let mut current_path = self.root_path.clone();
        let mut components = relative.components().peekable();
        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                return Err(ConfinedReadError::Invalid);
            };
            current_path.push(name);
            let is_last = components.peek().is_none();
            if is_last {
                let file = open_windows_path(&current_path, false)?;
                return read_file(file);
            }
            pinned_directories.push(open_windows_path(&current_path, true)?);
        }
        Err(ConfinedReadError::Invalid)
    }
}

fn validate_relative(relative: &Path) -> Result<(), ConfinedReadError> {
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ConfinedReadError::Invalid);
    }
    Ok(())
}

fn read_file(mut file: File) -> Result<Vec<u8>, ConfinedReadError> {
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(map_io_error)?;
    Ok(bytes)
}

fn map_io_error(error: io::Error) -> ConfinedReadError {
    match error.kind() {
        io::ErrorKind::NotFound => ConfinedReadError::Missing,
        _ => ConfinedReadError::Io,
    }
}

#[cfg(unix)]
fn os_str_cstring(value: &OsStr) -> Result<CString, ConfinedReadError> {
    CString::new(value.as_bytes()).map_err(|_| ConfinedReadError::Invalid)
}

#[cfg(unix)]
fn open_unix_path(path: &Path, flags: libc::c_int) -> Result<File, ConfinedReadError> {
    let value = os_str_cstring(path.as_os_str())?;
    // SAFETY: `value` is a live NUL-terminated pathname, flags request an existing read-only
    // object, and the returned descriptor is immediately transferred into `File` ownership.
    let fd = unsafe { libc::open(value.as_ptr(), flags) };
    owned_unix_file(fd)
}

#[cfg(unix)]
fn open_unix_at(
    parent: &File,
    name: &OsStr,
    flags: libc::c_int,
) -> Result<File, ConfinedReadError> {
    let value = os_str_cstring(name)?;
    // SAFETY: `parent` owns a live directory descriptor for the entire call, `value` is a single
    // validated relative component, and O_NOFOLLOW prevents the opened component from becoming a
    // symlink traversal. The returned descriptor is immediately transferred into `File` ownership.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), value.as_ptr(), flags) };
    if fd < 0 {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(code) if code == libc::ELOOP => Err(ConfinedReadError::Symlink),
            Some(code) if code == libc::ENOENT => Err(ConfinedReadError::Missing),
            Some(code) if code == libc::ENOTDIR => {
                let mut metadata = MaybeUninit::<libc::stat>::uninit();
                // SAFETY: `parent` owns a live directory descriptor, `value` is a live
                // NUL-terminated single path component, and `metadata` points to valid writable
                // storage. AT_SYMLINK_NOFOLLOW inspects the component itself rather than following
                // it, preserving the descriptor-relative confinement boundary.
                let result = unsafe {
                    libc::fstatat(
                        parent.as_raw_fd(),
                        value.as_ptr(),
                        metadata.as_mut_ptr(),
                        libc::AT_SYMLINK_NOFOLLOW,
                    )
                };
                if result == 0 {
                    // SAFETY: fstatat returned success and initialized the whole stat structure.
                    let metadata = unsafe { metadata.assume_init() };
                    if metadata.st_mode & libc::S_IFMT == libc::S_IFLNK {
                        return Err(ConfinedReadError::Symlink);
                    }
                }
                Err(ConfinedReadError::Invalid)
            }
            _ => Err(ConfinedReadError::Io),
        };
    }
    // SAFETY: `fd` was just returned successfully by openat, is uniquely owned here, and must be
    // closed exactly once. `File` assumes that ownership and closes it on drop.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn owned_unix_file(fd: libc::c_int) -> Result<File, ConfinedReadError> {
    if fd < 0 {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(code) if code == libc::ELOOP => Err(ConfinedReadError::Symlink),
            Some(code) if code == libc::ENOENT => Err(ConfinedReadError::Missing),
            Some(code) if code == libc::ENOTDIR => Err(ConfinedReadError::Invalid),
            _ => Err(ConfinedReadError::Io),
        };
    }
    // SAFETY: `fd` was just returned successfully by open, is uniquely owned here, and must be
    // closed exactly once. `File` assumes that ownership and closes it on drop.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(windows)]
fn open_windows_path(path: &Path, directory: bool) -> Result<File, ConfinedReadError> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(
            FILE_FLAG_OPEN_REPARSE_POINT
                | if directory {
                    FILE_FLAG_BACKUP_SEMANTICS
                } else {
                    0
                },
        );
    let file = options.open(path).map_err(map_io_error)?;
    let metadata = file.metadata().map_err(map_io_error)?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(ConfinedReadError::Symlink);
    }
    if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
        return Err(ConfinedReadError::Invalid);
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_only_normal_relative_paths() {
        let directory = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(directory.path().join("assets")).expect("assets");
        std::fs::write(directory.path().join("assets/site.css"), b"body {}").expect("write");
        let confined = ConfinedDir::open(directory.path()).expect("open root");
        assert_eq!(
            confined.read(Path::new("assets/site.css")).expect("read"),
            b"body {}"
        );
        assert_eq!(
            confined.read(Path::new("../secret.txt")),
            Err(ConfinedReadError::Invalid)
        );
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_relative_reads_do_not_follow_swapped_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path().join("root");
        let outside = directory.path().join("outside");
        std::fs::create_dir_all(root.join("assets")).expect("root");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(root.join("assets/site.css"), b"safe").expect("safe");
        std::fs::write(outside.join("site.css"), b"secret").expect("secret");

        let confined = ConfinedDir::open(&root).expect("open root");
        std::fs::rename(root.join("assets"), root.join("assets-old")).expect("rename");
        symlink(&outside, root.join("assets")).expect("symlink");

        assert_eq!(
            confined.read(Path::new("assets/site.css")),
            Err(ConfinedReadError::Symlink)
        );
    }
}
