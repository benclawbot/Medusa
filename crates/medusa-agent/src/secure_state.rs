use std::{fs, io, path::Path};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const DIRECTORY_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;

pub(crate) fn create_dir_all(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    secure_directory(path)?;
    secure_medusa_ancestor(path)
}

pub(crate) fn create_new_file(path: &Path) -> io::Result<fs::File> {
    open_secure_file(path, true)
}

pub(crate) fn create_file(path: &Path) -> io::Result<fs::File> {
    open_secure_file(path, false)
}

fn open_secure_file(path: &Path, create_new: bool) -> io::Result<fs::File> {
    if let Some(parent) = path.parent() {
        create_dir_all(parent)?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true);
    if create_new {
        options.create_new(true);
    } else {
        options.create(true).truncate(true);
    }
    #[cfg(unix)]
    options.mode(FILE_MODE);
    let file = options.open(path)?;
    secure_file(path)?;
    Ok(file)
}

pub(crate) fn repair(path: &Path, directory: bool) -> io::Result<()> {
    if directory {
        secure_directory(path)?;
        secure_medusa_ancestor(path)
    } else {
        secure_file(path)
    }
}

fn secure_medusa_ancestor(path: &Path) -> io::Result<()> {
    for ancestor in path.ancestors().skip(1) {
        if ancestor.file_name().is_some_and(|name| name == ".medusa") {
            return secure_directory(ancestor);
        }
    }
    Ok(())
}

fn secure_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(DIRECTORY_MODE))?;
    }
    #[cfg(windows)]
    {
        medusa_process_containment::secure_current_user_only(path, true)?;
    }
    Ok(())
}

fn secure_file(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(FILE_MODE))?;
    }
    #[cfg(windows)]
    {
        medusa_process_containment::secure_current_user_only(path, false)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn sensitive_state_is_owner_only_and_repairs_broad_modes() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let directory = temporary.path().join("state");
        create_dir_all(&directory).expect("secure directory");
        let file_path = directory.join("session.json");
        drop(create_new_file(&file_path).expect("secure file"));

        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            DIRECTORY_MODE
        );
        assert_eq!(
            fs::metadata(&file_path).unwrap().permissions().mode() & 0o777,
            FILE_MODE
        );

        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&file_path, fs::Permissions::from_mode(0o644)).unwrap();
        repair(&directory, true).expect("repair directory");
        repair(&file_path, false).expect("repair file");
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            DIRECTORY_MODE
        );
        assert_eq!(
            fs::metadata(&file_path).unwrap().permissions().mode() & 0o777,
            FILE_MODE
        );
    }

    #[cfg(unix)]
    #[test]
    fn replaceable_secure_file_recovers_a_stale_temp_file() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("state.tmp");
        fs::write(&path, b"stale").expect("stale temp");
        let mut file = create_file(&path).expect("replace stale temp");
        use std::io::Write as _;
        file.write_all(b"fresh").expect("fresh content");
        drop(file);
        assert_eq!(fs::read(&path).expect("temp content"), b"fresh");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            FILE_MODE
        );
    }

    #[cfg(unix)]
    #[test]
    fn securing_a_child_also_protects_the_medusa_root() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join(".medusa");
        let child = root.join("sessions");
        create_dir_all(&child).expect("secure child");

        assert_eq!(
            fs::metadata(root).unwrap().permissions().mode() & 0o777,
            DIRECTORY_MODE
        );
    }

    #[cfg(windows)]
    #[test]
    fn sensitive_state_uses_current_user_only_acl() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let directory = temporary.path().join("state");
        create_dir_all(&directory).expect("secure directory");
        medusa_process_containment::verify_current_user_only(&directory, true)
            .expect("owner-only directory ACL");
        let file_path = directory.join("session.json");
        drop(create_new_file(&file_path).expect("secure file"));
        medusa_process_containment::verify_current_user_only(&file_path, false)
            .expect("owner-only file ACL");
    }
}
