use std::{
    collections::BTreeSet,
    env, io,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug)]
pub(crate) struct UnixSandboxInputs {
    pub(crate) executable: PathBuf,
    pub(crate) read_only_roots: Vec<PathBuf>,
    pub(crate) path: String,
    pub(crate) cargo_home: Option<PathBuf>,
    pub(crate) rustup_home: Option<PathBuf>,
}

pub(crate) fn inputs(program: &str) -> io::Result<UnixSandboxInputs> {
    let executable = resolve_program(program)?;
    let home = env::var_os("HOME").map(PathBuf::from);
    let cargo_home = runtime_home("CARGO_HOME", ".cargo", home.as_deref());
    let rustup_home = runtime_home("RUSTUP_HOME", ".rustup", home.as_deref());

    let mut path_entries = Vec::new();
    let mut read_only_roots = BTreeSet::new();
    if let Some(parent) = executable.parent() {
        read_only_roots.insert(parent.to_path_buf());
    }
    #[cfg(target_os = "macos")]
    if let Some(runtime_root) = macos_python_runtime_root(&executable) {
        read_only_roots.insert(runtime_root);
    }

    if let Some(path) = env::var_os("PATH") {
        for entry in env::split_paths(&path) {
            let Ok(entry) = entry.canonicalize() else {
                continue;
            };
            if !entry.is_dir() || home.as_ref().is_some_and(|home| entry == *home) {
                continue;
            }
            if !path_entries.contains(&entry) {
                path_entries.push(entry.clone());
            }
            read_only_roots.insert(entry);
        }
    }

    // System runtime roots are deliberately kept at their original absolute paths.
    // Canonicalizing /lib or /lib64 can turn them into /usr/... and remove the
    // loader path encoded in ELF binaries from the empty-root Linux namespace.
    for root in system_runtime_roots() {
        let root = Path::new(root);
        if root.exists() {
            read_only_roots.insert(root.to_path_buf());
        }
    }
    for root in [&cargo_home, &rustup_home].into_iter().flatten() {
        if let Ok(root) = root.canonicalize() {
            read_only_roots.insert(root);
        }
    }

    if path_entries.is_empty()
        && let Some(parent) = executable.parent()
    {
        path_entries.push(parent.to_path_buf());
    }
    let path = env::join_paths(&path_entries)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?
        .to_string_lossy()
        .into_owned();

    Ok(UnixSandboxInputs {
        executable,
        read_only_roots: collapse_roots(read_only_roots),
        path,
        cargo_home,
        rustup_home,
    })
}

#[cfg(target_os = "linux")]
pub(crate) fn linux_command(root: &Path, program: &str, args: &[String]) -> io::Result<Command> {
    let inputs = inputs(program)?;
    let mut command = Command::new("bwrap");
    command.args([
        "--die-with-parent",
        "--new-session",
        "--unshare-user",
        "--uid",
        "0",
        "--gid",
        "0",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-net",
        "--tmpfs",
        "/",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
    ]);

    for read_only in &inputs.read_only_roots {
        command.arg("--ro-bind").arg(read_only).arg(read_only);
    }
    command
        .arg("--bind")
        .arg(root)
        .arg(root)
        .arg("--chdir")
        .arg(root)
        .args(["--clearenv", "--setenv", "PATH"])
        .arg(&inputs.path)
        .args(["--setenv", "HOME"])
        .arg(root)
        .args(["--setenv", "TMPDIR", "/tmp"])
        .args(["--setenv", "PYTHONDONTWRITEBYTECODE", "1"]);
    if let Some(cargo_home) = &inputs.cargo_home {
        command.args(["--setenv", "CARGO_HOME"]).arg(cargo_home);
    }
    if let Some(rustup_home) = &inputs.rustup_home {
        command.args(["--setenv", "RUSTUP_HOME"]).arg(rustup_home);
    }
    command.arg("--").arg(&inputs.executable).args(args);
    Ok(command)
}

#[cfg(target_os = "macos")]
pub(crate) fn macos_command(
    root: &Path,
    program: &str,
    args: &[String],
    profile_path: &Path,
) -> io::Result<Command> {
    let inputs = inputs(program)?;
    let profile = macos_profile(root, &inputs.read_only_roots);
    std::fs::write(profile_path, profile)?;

    let mut command = Command::new("sandbox-exec");
    command
        .arg("-f")
        .arg(profile_path)
        .arg(&inputs.executable)
        .args(args)
        .current_dir(root)
        .env_clear()
        .env("PATH", &inputs.path)
        .env("HOME", root)
        .env("TMPDIR", "/tmp")
        .env("PYTHONDONTWRITEBYTECODE", "1");
    if let Some(cargo_home) = &inputs.cargo_home {
        command.env("CARGO_HOME", cargo_home);
    }
    if let Some(rustup_home) = &inputs.rustup_home {
        command.env("RUSTUP_HOME", rustup_home);
    }
    Ok(command)
}

#[cfg(target_os = "macos")]
fn macos_profile(root: &Path, read_only_roots: &[PathBuf]) -> String {
    let mut profile = String::from(
        "(version 1)\n(deny default)\n(allow process-exec* process-fork)\n(allow file-read*\n",
    );
    profile.push_str(&format!("  (subpath \"{}\")\n", profile_path(root)));
    for read_only in read_only_roots {
        profile.push_str(&format!("  (subpath \"{}\")\n", profile_path(read_only)));
    }
    profile.push_str(")\n");
    profile.push_str(&format!(
        "(allow file-write* (subpath \"{}\") (subpath \"/tmp\") (subpath \"/private/tmp\"))\n",
        profile_path(root)
    ));
    profile.push_str("(deny network*)\n");
    profile
}

#[cfg(target_os = "macos")]
fn profile_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

#[cfg(target_os = "macos")]
fn macos_python_runtime_root(executable: &Path) -> Option<PathBuf> {
    let name = executable.file_name()?.to_string_lossy().to_ascii_lowercase();
    if !name.starts_with("python") {
        return None;
    }
    let bin = executable.parent()?;
    if bin.file_name().and_then(|name| name.to_str()) != Some("bin") {
        return None;
    }
    bin.parent().map(Path::to_path_buf)
}

fn resolve_program(program: &str) -> io::Result<PathBuf> {
    let requested = Path::new(program);
    if requested.components().count() > 1 {
        return requested.canonicalize().and_then(require_file);
    }

    let Some(path) = env::var_os("PATH") else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("sandbox executable was not found on PATH: {program}"),
        ));
    };
    for directory in env::split_paths(&path) {
        let candidate = directory.join(program);
        if candidate.is_file() {
            return candidate.canonicalize().and_then(require_file);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("sandbox executable was not found on PATH: {program}"),
    ))
}

fn require_file(path: PathBuf) -> io::Result<PathBuf> {
    if path.is_file() {
        Ok(path)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("sandbox executable is not a file: {}", path.display()),
        ))
    }
}

fn runtime_home(variable: &str, fallback: &str, home: Option<&Path>) -> Option<PathBuf> {
    env::var_os(variable)
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .or_else(|| {
            home.map(|home| home.join(fallback))
                .filter(|path| path.is_dir())
        })
}

fn collapse_roots(roots: BTreeSet<PathBuf>) -> Vec<PathBuf> {
    let mut collapsed: Vec<PathBuf> = Vec::new();
    for candidate in roots {
        if collapsed.iter().any(|root| candidate.starts_with(root)) {
            continue;
        }
        collapsed.retain(|root| !root.starts_with(&candidate));
        collapsed.push(candidate);
    }
    collapsed
}

fn system_runtime_roots() -> &'static [&'static str] {
    #[cfg(target_os = "linux")]
    {
        &[
            "/usr",
            "/lib",
            "/lib64",
            "/etc/alternatives",
            "/etc/ld.so.cache",
            "/etc/localtime",
            "/etc/ssl",
        ]
    }
    #[cfg(target_os = "macos")]
    {
        &[
            "/System",
            "/usr",
            "/bin",
            "/sbin",
            "/Library/Apple",
            "/Library/Developer",
            "/Applications/Xcode.app",
            "/opt/homebrew",
            "/usr/local",
            "/private/etc/ssl",
            "/private/etc/paths.d",
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_roots_never_grant_the_entire_filesystem() {
        let executable = resolve_program("true").expect("true must be available for sandbox tests");
        let inputs =
            inputs(executable.to_str().expect("UTF-8 executable path")).expect("sandbox inputs");
        assert!(
            inputs
                .read_only_roots
                .iter()
                .all(|path| path != Path::new("/"))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_loader_alias_roots_are_preserved_without_canonicalization() {
        let inputs = inputs("true").expect("sandbox inputs");
        for loader_root in ["/lib", "/lib64"] {
            let loader_root = Path::new(loader_root);
            if loader_root.exists() {
                assert!(
                    inputs.read_only_roots.contains(&loader_root.to_path_buf()),
                    "Linux loader root {} must remain visible at its original path",
                    loader_root.display()
                );
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_python_runtime_prefix_is_read_only_visible() {
        let executable = Path::new(
            "/Users/runner/hostedtoolcache/Python/3.13.5/arm64/bin/python3",
        );
        assert_eq!(
            macos_python_runtime_root(executable),
            Some(PathBuf::from(
                "/Users/runner/hostedtoolcache/Python/3.13.5/arm64"
            ))
        );
        assert_eq!(
            macos_python_runtime_root(Path::new("/Users/runner/.local/bin/node")),
            None
        );
    }

    #[test]
    fn parent_home_is_not_copied_into_child_environment() {
        let executable = resolve_program("true").expect("true must be available");
        let inputs =
            inputs(executable.to_str().expect("UTF-8 executable path")).expect("sandbox inputs");
        let home = env::var_os("HOME").map(PathBuf::from);
        assert!(
            home.as_ref()
                .is_none_or(|home| !inputs.read_only_roots.contains(home)),
            "the ambient HOME must not become a blanket read root"
        );
    }
}
