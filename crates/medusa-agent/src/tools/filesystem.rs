use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use walkdir::WalkDir;

use crate::{
    policy::safe_path,
    transaction::{
        FileMutation, MutationContext, TransactionOutcome, apply_atomic, apply_atomic_with_context,
    },
};

const MAX_SEARCH_FILES: usize = 10_000;
const MAX_SEARCH_BYTES: u64 = 32 * 1024 * 1024;
static APPROVED_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
const IGNORED_DIRECTORY_NAMES: &[&str] = &[
    ".git",
    ".medusa",
    "target",
    "node_modules",
    ".venv",
    "venv",
    "dist",
    "build",
    ".next",
    "coverage",
];

fn is_ignored_directory(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        IGNORED_DIRECTORY_NAMES
            .iter()
            .any(|ignored| name == *ignored)
    })
}

pub(crate) fn read(repo: &Path, relative: &str) -> MedusaResult<String> {
    if relative == "." {
        return Ok(repository_listing(repo));
    }
    let path = safe_path(repo, relative)?;
    fs::read_to_string(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            MedusaError::new(
                ErrorCode::InvalidInput,
                ErrorCategory::Validation,
                format!("repository path does not exist: {relative}"),
            )
        } else {
            error.into()
        }
    })
}

fn repository_listing(repo: &Path) -> String {
    const MAX_ENTRIES: usize = 80;
    let mut entries = WalkDir::new(repo)
        .min_depth(1)
        .max_depth(2)
        .into_iter()
        .filter_entry(|entry| !is_ignored_directory(entry.path()))
        .filter_map(Result::ok)
        .filter(|entry| !is_ignored_directory(entry.path()))
        .filter_map(|entry| {
            let relative = entry.path().strip_prefix(repo).ok()?;
            let mut display = relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            if entry.file_type().is_dir() {
                display.push('/');
            }
            Some(display)
        })
        .take(MAX_ENTRIES)
        .collect::<Vec<_>>();
    entries.sort();
    if entries.len() == MAX_ENTRIES {
        entries.push("... listing truncated".to_owned());
    }
    entries.join("\n")
}

/// Rejects mutations aimed at the Git metadata directory.
///
/// Repository-relative writes are otherwise unattended, and `.git/hooks`
/// entries execute on the next Git invocation, which would let a repository
/// write escalate into code execution outside the command sandbox. Reads are
/// unaffected so that Git state remains inspectable.
fn reject_git_metadata(relative: &str) -> MedusaResult<()> {
    let first = Path::new(relative)
        .components()
        .find_map(|component| match component {
            std::path::Component::Normal(name) => name.to_str(),
            _ => None,
        });
    if first.is_some_and(|name| name.eq_ignore_ascii_case(".git")) {
        return Err(medusa_core::MedusaError::new(
            medusa_core::ErrorCode::PolicyDenied,
            medusa_core::ErrorCategory::Policy,
            format!("refusing to modify Git metadata: {relative}"),
        ));
    }
    Ok(())
}

pub(crate) fn write(repo: &Path, relative: &str, content: &str) -> MedusaResult<String> {
    reject_git_metadata(relative)?;
    apply_atomic(
        repo,
        &[FileMutation {
            path: relative.to_owned(),
            content: content.to_owned(),
        }],
    )?;
    Ok(format!("wrote {} bytes to {relative}", content.len()))
}

pub(crate) fn write_with_context(
    repo: &Path,
    relative: &str,
    content: &str,
    context: &MutationContext,
) -> MedusaResult<TransactionOutcome> {
    reject_git_metadata(relative)?;
    apply_atomic_with_context(
        repo,
        &[FileMutation {
            path: relative.to_owned(),
            content: content.to_owned(),
        }],
        context,
    )
}

pub(crate) fn create_dir(repo: &Path, relative: &str) -> MedusaResult<String> {
    reject_git_metadata(relative)?;
    let path = safe_path(repo, relative)?;
    fs::create_dir_all(&path)?;
    Ok(format!("created directory {}", path.display()))
}

pub(crate) fn write_approved(path: &str, content: &str) -> MedusaResult<String> {
    let path = approved_absolute_path(path)?;
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "approved path has no parent directory",
        )
    })?;
    fs::create_dir_all(parent)?;
    let original_permissions = fs::symlink_metadata(&path)
        .ok()
        .filter(|metadata| !metadata.file_type().is_symlink())
        .map(|metadata| metadata.permissions());
    let (temporary, mut file) = create_approved_temporary(parent, &path)?;
    let result = (|| {
        file.write_all(content.as_bytes())?;
        if let Some(permissions) = original_permissions {
            fs::set_permissions(&temporary, permissions)?;
        }
        file.sync_all()?;
        drop(file);
        // Recheck immediately before publication. A destination that became a
        // symlink after approval must never be followed or replaced.
        if fs::symlink_metadata(&path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "approved destination became a symbolic link",
            ));
        }
        replace_approved_file(&temporary, &path)?;
        sync_parent_directory(parent);
        Ok::<(), std::io::Error>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    Ok(format!(
        "wrote {} bytes to {}",
        content.len(),
        path.display()
    ))
}

fn create_approved_temporary(parent: &Path, destination: &Path) -> MedusaResult<(PathBuf, File)> {
    let name = destination
        .file_name()
        .map(|value| value.to_string_lossy())
        .unwrap_or_else(|| std::borrow::Cow::Borrowed("medusa"));
    for _ in 0..16 {
        let sequence = APPROVED_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".{name}.{sequence}.medusa-approved-tmp"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(medusa_core::MedusaError::new(
        ErrorCode::PersistenceFailed,
        ErrorCategory::Persistence,
        "could not allocate a unique approved-write temporary path",
    ))
}

#[cfg(not(windows))]
fn replace_approved_file(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(temporary, destination)
}

#[cfg(windows)]
fn replace_approved_file(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    match fs::rename(temporary, destination) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // Windows std::fs::rename cannot replace an existing file. Move the
            // old inode aside first, then publish the staged inode. If publish
            // fails, restore the old inode before returning the error.
            let parent = destination.parent().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "destination has no parent",
                )
            })?;
            let name = destination
                .file_name()
                .map(|value| value.to_string_lossy())
                .unwrap_or_else(|| std::borrow::Cow::Borrowed("medusa"));
            let mut backup = None;
            for sequence in 0..16_u32 {
                let candidate = parent.join(format!(".{name}.{sequence}.medusa-approved-backup"));
                match fs::rename(destination, &candidate) {
                    Ok(()) => {
                        backup = Some(candidate);
                        break;
                    }
                    Err(rename_error)
                        if rename_error.kind() == std::io::ErrorKind::AlreadyExists =>
                    {
                        continue;
                    }
                    Err(rename_error) => return Err(rename_error),
                }
            }
            let backup = backup.ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "could not allocate an approved-write backup path",
                )
            })?;
            match fs::rename(temporary, destination) {
                Ok(()) => {
                    let _ = fs::remove_file(backup);
                    Ok(())
                }
                Err(publish_error) => {
                    let _ = fs::rename(&backup, destination);
                    Err(publish_error)
                }
            }
        }
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) {
    if let Ok(directory) = fs::File::open(parent) {
        let _ = directory.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) {}

pub(crate) fn create_dir_approved(path: &str) -> MedusaResult<String> {
    let path = approved_absolute_path(path)?;
    fs::create_dir_all(&path)?;
    Ok(format!("created directory {}", path.display()))
}

fn normalized_policy_path(path: &Path) -> String {
    let path = canonicalize_existing_prefix(path);
    let normalized = path.to_string_lossy().replace('\\', "/");
    let normalized = if let Some(suffix) = normalized.strip_prefix("//?/UNC/") {
        format!("//{suffix}")
    } else if let Some(suffix) = normalized.strip_prefix("//?/") {
        suffix.to_owned()
    } else {
        normalized
    };
    let normalized = normalized.trim_end_matches('/');
    if cfg!(any(windows, target_os = "macos")) {
        normalized.to_ascii_lowercase()
    } else {
        normalized.to_owned()
    }
}

fn canonicalize_existing_prefix(path: &Path) -> std::path::PathBuf {
    let mut existing = path;
    let mut suffix = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name() else {
            return path.to_path_buf();
        };
        suffix.push(name.to_os_string());
        let Some(parent) = existing.parent() else {
            return path.to_path_buf();
        };
        existing = parent;
    }
    let Ok(mut canonical) = existing.canonicalize() else {
        return path.to_path_buf();
    };
    for component in suffix.iter().rev() {
        canonical.push(component);
    }
    canonical
}

fn path_is_at_or_below(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn reject_sensitive_approved_path(path: &Path) -> MedusaResult<()> {
    use medusa_core::{ErrorCategory, ErrorCode, MedusaError};

    let normalized = normalized_policy_path(path);
    let components = normalized
        .split('/')
        .filter(|component| !component.is_empty());
    if components
        .clone()
        .any(|component| component.eq_ignore_ascii_case(".git"))
    {
        return Err(MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            format!(
                "approved external path is sensitive and cannot be modified: {}",
                path.display()
            ),
        ));
    }

    let mut sensitive_prefixes = vec![
        "/etc".to_owned(),
        "/bin".to_owned(),
        "/sbin".to_owned(),
        "/usr/bin".to_owned(),
        "/usr/sbin".to_owned(),
        "/usr/local/bin".to_owned(),
        "/usr/local/sbin".to_owned(),
        "/library/launchagents".to_owned(),
        "/library/launchdaemons".to_owned(),
        "c:/windows/system32/drivers/etc".to_owned(),
        "c:/windows/system32/config".to_owned(),
        "c:/windows/system32/wbem".to_owned(),
        "c:/windows/system32".to_owned(),
        "c:/windows/syswow64".to_owned(),
    ];

    for home in [std::env::var_os("HOME"), std::env::var_os("USERPROFILE")]
        .into_iter()
        .flatten()
    {
        let home = normalized_policy_path(Path::new(&home));
        for suffix in [
            ".ssh",
            ".aws/credentials",
            ".gnupg",
            ".config/gh/hosts.yml",
            ".config/autostart",
            "library/launchagents",
            "appdata/roaming/microsoft/windows/start menu/programs/startup",
        ] {
            sensitive_prefixes.push(format!("{home}/{suffix}"));
        }
    }

    if sensitive_prefixes
        .iter()
        .any(|prefix| path_is_at_or_below(&normalized, prefix))
    {
        return Err(MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            format!(
                "approved external path is sensitive and cannot be modified: {}",
                path.display()
            ),
        ));
    }

    Ok(())
}

fn approved_absolute_path(value: &str) -> MedusaResult<std::path::PathBuf> {
    use medusa_core::{ErrorCategory, ErrorCode, MedusaError};

    let path = Path::new(value);
    if !path.is_absolute() || path.parent().is_none() {
        return Err(MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            "an approved external path must be absolute and narrower than a filesystem root",
        ));
    }
    // Resolve against an existing real directory. A regular-file target is a
    // valid replacement destination, so it must be treated as a suffix rather
    // than as the canonicalization root; canonicalizing the file itself can
    // make its parent appear to be a file and produce ENOTDIR during writes.
    // Symlink ancestors are rejected while walking so an approved path cannot
    // escape through a directory link between approval and publication.
    let mut existing = path;
    let mut suffix = Vec::new();
    loop {
        match fs::symlink_metadata(existing) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(MedusaError::new(
                    ErrorCode::PolicyDenied,
                    ErrorCategory::Policy,
                    "approved path traverses a symbolic link",
                ));
            }
            Ok(metadata) if metadata.is_dir() => break,
            Ok(_) => {
                let name = existing.file_name().ok_or_else(|| {
                    MedusaError::new(
                        ErrorCode::PolicyDenied,
                        ErrorCategory::Policy,
                        "approved path has no existing confined ancestor",
                    )
                })?;
                suffix.push(name.to_os_string());
                existing = existing.parent().ok_or_else(|| {
                    MedusaError::new(
                        ErrorCode::PolicyDenied,
                        ErrorCategory::Policy,
                        "approved path has no existing confined ancestor",
                    )
                })?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = existing.file_name().ok_or_else(|| {
                    MedusaError::new(
                        ErrorCode::PolicyDenied,
                        ErrorCategory::Policy,
                        "approved path has no existing confined ancestor",
                    )
                })?;
                suffix.push(name.to_os_string());
                existing = existing.parent().ok_or_else(|| {
                    MedusaError::new(
                        ErrorCode::PolicyDenied,
                        ErrorCategory::Policy,
                        "approved path has no existing confined ancestor",
                    )
                })?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    let mut resolved = existing.canonicalize()?;
    for component in suffix.iter().rev() {
        resolved.push(component);
    }
    reject_sensitive_approved_path(&resolved)?;
    if resolved.exists() && fs::symlink_metadata(&resolved)?.file_type().is_symlink() {
        return Err(MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            "approved path targets a symbolic link",
        ));
    }
    Ok(resolved)
}

pub(crate) fn search(repo: &Path, query: &str) -> MedusaResult<String> {
    let mut results = Vec::new();
    let mut scanned_files = 0usize;
    let mut scanned_bytes = 0u64;
    let mut truncated = false;
    for entry in WalkDir::new(repo)
        .into_iter()
        .filter_entry(|entry| !is_ignored_directory(entry.path()))
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        scanned_files = scanned_files.saturating_add(1);
        let bytes = entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        if scanned_files > MAX_SEARCH_FILES
            || scanned_bytes.saturating_add(bytes) > MAX_SEARCH_BYTES
        {
            truncated = true;
            break;
        }
        scanned_bytes = scanned_bytes.saturating_add(bytes);
        if let Ok(text) = fs::read_to_string(entry.path()) {
            for (index, line) in text.lines().enumerate() {
                if line.contains(query) {
                    let relative = entry.path().strip_prefix(repo).unwrap_or(entry.path());
                    let relative = relative
                        .components()
                        .map(|part| part.as_os_str().to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/");
                    results.push(format!("{}:{}:{}", relative, index + 1, line.trim()));
                }
            }
        }
    }
    let mut output = results.join("\n");
    if truncated {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&format!(
            "[search truncated after scanning {scanned_files} files or {scanned_bytes} bytes]"
        ));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, thread};

    use medusa_core::{ErrorCategory, ErrorCode};

    use super::{
        approved_absolute_path, create_dir, normalized_policy_path, read,
        reject_sensitive_approved_path, search, write, write_approved,
    };

    #[test]
    fn extracted_filesystem_tools_preserve_read_write_and_search_behavior() {
        let directory = tempfile::tempdir().expect("tempdir");
        let directory_receipt =
            create_dir(directory.path(), "nested/assets").expect("create nested directory");
        assert!(directory_receipt.contains("nested"));
        assert!(directory.path().join("nested/assets").is_dir());
        let receipt =
            write(directory.path(), "nested/value.txt", "alpha\nbeta\n").expect("atomic write");
        assert!(receipt.contains("11 bytes"));
        assert_eq!(
            read(directory.path(), "nested/value.txt").expect("read"),
            "alpha\nbeta\n"
        );

        let listing = read(directory.path(), ".").expect("repository listing");
        assert!(listing.contains("nested/"));
        assert!(listing.contains("nested/value.txt"));

        fs::create_dir_all(directory.path().join(".medusa")).expect("medusa dir");
        fs::write(directory.path().join(".medusa/hidden.txt"), "alpha").expect("hidden fixture");
        let matches = search(directory.path(), "beta").expect("search");
        assert!(matches.contains("nested/value.txt:2:beta"));
        assert!(!matches.contains("hidden.txt"));
    }

    #[test]
    fn approved_write_uses_an_exclusive_unique_temporary_and_preserves_canaries() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let target = directory.path().join("approved.txt");
        let fixed_name = target.with_extension("medusa-approved-tmp");
        let canary = directory.path().join("canary.txt");
        fs::write(&canary, "canary").expect("canary");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&canary, &fixed_name).expect("temporary symlink canary");
        #[cfg(not(unix))]
        fs::write(&fixed_name, "temporary canary").expect("temporary canary");

        write_approved(&target.to_string_lossy(), "approved").expect("approved write");

        assert_eq!(fs::read_to_string(&target).expect("target"), "approved");
        assert_eq!(fs::read_to_string(&canary).expect("canary"), "canary");
        #[cfg(unix)]
        assert!(
            fs::symlink_metadata(&fixed_name)
                .expect("temporary path")
                .file_type()
                .is_symlink()
        );
        #[cfg(not(unix))]
        assert_eq!(
            fs::read_to_string(&fixed_name).expect("temporary canary"),
            "temporary canary"
        );
    }

    #[cfg(unix)]
    #[test]
    fn approved_write_preserves_existing_mode() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary directory");
        let target = directory.path().join("executable");
        fs::write(&target, "old").expect("target");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o751)).expect("mode");

        write_approved(&target.to_string_lossy(), "new").expect("approved write");

        assert_eq!(fs::read_to_string(&target).expect("target"), "new");
        assert_eq!(
            fs::metadata(&target)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o751
        );
    }

    #[test]
    fn concurrent_reads_report_missing_repository_paths_as_input_errors() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(directory.path().join("src")).expect("src fixture");
        fs::write(directory.path().join("src/lib.rs"), "pub fn value() {}\n").expect("source");
        fs::write(
            directory.path().join("Cargo.toml"),
            "[package]\nname='fixture'\n",
        )
        .expect("manifest");

        let paths = [
            "src/lib.rs",
            "Cargo.toml",
            "rust-toolchain.toml",
            ".cargo/config.toml",
        ];
        let results = thread::scope(|scope| {
            paths
                .iter()
                .map(|path| scope.spawn(|| (*path, read(directory.path(), path))))
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| handle.join().expect("read thread"))
                .collect::<Vec<_>>()
        });

        assert!(
            results[0]
                .1
                .as_ref()
                .is_ok_and(|value| value.contains("value"))
        );
        assert!(
            results[1]
                .1
                .as_ref()
                .is_ok_and(|value| value.contains("package"))
        );
        for (path, result) in &results[2..] {
            let error = result.as_ref().expect_err("missing read must fail");
            assert_eq!(error.code, ErrorCode::InvalidInput, "{path}");
            assert_eq!(error.category, ErrorCategory::Validation, "{path}");
            assert_eq!(
                error.message,
                format!("repository path does not exist: {path}")
            );
            assert_ne!(error.code, ErrorCode::PersistenceFailed, "{path}");
        }
    }

    #[test]
    fn extracted_filesystem_tools_reject_parent_traversal() {
        let directory = tempfile::tempdir().expect("tempdir");
        assert!(read(directory.path(), "../secret.txt").is_err());
        assert!(write(directory.path(), "../secret.txt", "nope").is_err());
        assert!(create_dir(directory.path(), "../outside").is_err());
    }

    #[test]
    fn approved_external_paths_reject_git_metadata() {
        assert!(
            reject_sensitive_approved_path(Path::new("/tmp/project/.git/hooks/pre-commit"))
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn approved_external_paths_reject_unix_system_targets() {
        for path in ["/etc/hosts", "/bin/tool", "/sbin/tool", "/usr/bin/tool"] {
            assert!(
                reject_sensitive_approved_path(Path::new(path)).is_err(),
                "{path}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn approved_external_paths_reject_windows_system_targets_after_canonicalization() {
        assert_eq!(
            super::normalized_policy_path(Path::new(r"\\?\C:\Windows\System32\drivers\etc\hosts")),
            "c:/windows/system32/drivers/etc/hosts"
        );

        let windows = std::env::var_os("WINDIR")
            .or_else(|| std::env::var_os("SystemRoot"))
            .expect("WINDIR or SystemRoot");
        let target = Path::new(&windows).join("System32/drivers/etc/hosts");
        assert!(
            approved_absolute_path(target.to_str().expect("utf8 system path")).is_err(),
            "{}",
            target.display()
        );
    }

    #[test]
    fn approved_external_paths_reject_user_credentials() {
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
        let Some(home) = home else {
            return;
        };
        let home = Path::new(&home);
        for suffix in [
            ".ssh/authorized_keys",
            ".aws/credentials",
            ".gnupg/private-keys-v1.d/key",
            ".config/gh/hosts.yml",
        ] {
            let path = home.join(suffix);
            assert!(
                reject_sensitive_approved_path(&path).is_err(),
                "{}",
                path.display()
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn approved_external_paths_reject_linux_autostart() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let path = Path::new(&home).join(".config/autostart/medusa.desktop");
        assert!(reject_sensitive_approved_path(&path).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn approved_external_paths_reject_macos_launch_agents_after_canonicalization() {
        let target = Path::new("/Library/LaunchAgents/com.medusa.agent.plist");
        assert!(approved_absolute_path(target.to_str().expect("utf8 path")).is_err());

        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let user_target = Path::new(&home).join("Library/LaunchAgents/com.medusa.agent.plist");
        assert!(
            reject_sensitive_approved_path(&user_target).is_err(),
            "{}",
            user_target.display()
        );
    }

    #[cfg(windows)]
    #[test]
    fn approved_external_paths_reject_windows_startup() {
        let Some(home) = std::env::var_os("USERPROFILE") else {
            return;
        };
        let target = Path::new(&home)
            .join("AppData/Roaming/Microsoft/Windows/Start Menu/Programs/Startup/medusa.cmd");
        assert!(reject_sensitive_approved_path(&target).is_err());
    }

    #[test]
    fn approved_external_path_outside_denylist_remains_allowed_by_path_policy() {
        let directory = tempfile::tempdir().expect("tempdir");
        let target = directory.path().join("exports/report.txt");
        assert_eq!(
            normalized_policy_path(
                &approved_absolute_path(target.to_str().expect("utf8 path")).expect("allowed")
            ),
            normalized_policy_path(&target)
        );
    }

    #[cfg(unix)]
    #[test]
    fn single_file_write_uses_the_same_symlink_boundary_as_transactions() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("outside");
        symlink(outside.path(), directory.path().join("linked")).expect("symlink");

        assert!(write(directory.path(), "linked/escape.txt", "nope").is_err());
        assert!(!outside.path().join("escape.txt").exists());
    }
}
