use std::{fs, path::Path};

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
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let original_permissions = fs::metadata(&path)
        .ok()
        .map(|metadata| metadata.permissions());
    let temporary = path.with_extension("medusa-approved-tmp");
    fs::write(&temporary, content)?;
    if let Some(permissions) = original_permissions {
        fs::set_permissions(&temporary, permissions)?;
    }
    fs::rename(&temporary, &path)?;
    Ok(format!(
        "wrote {} bytes to {}",
        content.len(),
        path.display()
    ))
}

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
    let mut existing = path;
    while !existing.exists() {
        existing = existing.parent().ok_or_else(|| {
            MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                "approved path has no existing confined ancestor",
            )
        })?;
    }
    let canonical_existing = existing.canonicalize()?;
    let suffix = path.strip_prefix(existing).map_err(|error| {
        MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            format!("approved path could not be confined: {error}"),
        )
    })?;
    let resolved = canonical_existing.join(suffix);
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
        reject_sensitive_approved_path, search, write,
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
