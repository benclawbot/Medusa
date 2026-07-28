use std::{fs, path::Path, process::Command, sync::Mutex};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use reqwest::blocking::Client;
use serde::Deserialize;

const GITHUB_API: &str = "https://api.github.com";
const REPOSITORY: &str = "benclawbot/Medusa";
const BRANCH: &str = "main";
const REPOSITORY_URL: &str = "https://github.com/benclawbot/Medusa.git";
const SHA1_HEX_LENGTH: usize = 40;
const SHA256_HEX_LENGTH: usize = 64;

/// The immutable revision currently at the head of Medusa's main branch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MainBranchRevision {
    pub sha: String,
}

/// Discovers main-branch revisions and schedules a source build after the caller exits.
pub struct MainBranchUpdater {
    client: Client,
    api_base: String,
    verified_revision: Mutex<Option<String>>,
}

impl MainBranchUpdater {
    pub fn public() -> MedusaResult<Self> {
        Self::new(GITHUB_API)
    }

    pub fn new(api_base: impl Into<String>) -> MedusaResult<Self> {
        Ok(Self {
            client: Client::builder()
                .user_agent("medusa-updater")
                .build()
                .map_err(http_error)?,
            api_base: api_base.into().trim_end_matches('/').to_owned(),
            verified_revision: Mutex::new(None),
        })
    }

    pub fn latest_main(&self) -> MedusaResult<MainBranchRevision> {
        let url = format!("{}/repos/{REPOSITORY}/commits/{BRANCH}", self.api_base);
        let revision: GithubCommit = self
            .client
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .map_err(http_error)?
            .error_for_status()
            .map_err(http_error)?
            .json()
            .map_err(http_error)?;
        validate_revision(&revision.sha)?;
        *self
            .verified_revision
            .lock()
            .map_err(|_| internal_error("verified update revision lock is poisoned"))? =
            Some(revision.sha.clone());
        Ok(MainBranchRevision { sha: revision.sha })
    }

    /// Starts a detached helper that waits for this CLI, builds the verified revision, and restarts Medusa.
    pub fn schedule_main_install(&self, executable: &Path, parent_pid: u32) -> MedusaResult<()> {
        ensure_cargo_available()?;
        let revision = self
            .verified_revision
            .lock()
            .map_err(|_| internal_error("verified update revision lock is poisoned"))?
            .clone()
            .ok_or_else(|| {
                MedusaError::new(
                    ErrorCode::InternalInvariant,
                    ErrorCategory::Internal,
                    "cannot schedule a main-branch install before verifying a revision",
                )
            })?;
        validate_revision(&revision)?;
        #[cfg(windows)]
        {
            let script = executable.with_extension("main-update.ps1");
            fs::write(
                &script,
                windows_source_script(parent_pid, executable, &script, &revision),
            )?;
            Command::new("powershell")
                .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
                .arg(&script)
                .spawn()
                .map_err(command_error)?;
        }
        #[cfg(not(windows))]
        {
            let script = executable.with_extension("main-update.sh");
            fs::write(
                &script,
                unix_source_script(parent_pid, executable, &script, &revision),
            )?;
            Command::new("sh")
                .arg(&script)
                .spawn()
                .map_err(command_error)?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct GithubCommit {
    sha: String,
}

fn validate_revision(revision: &str) -> MedusaResult<()> {
    if !matches!(revision.len(), SHA1_HEX_LENGTH | SHA256_HEX_LENGTH)
        || !revision.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            "GitHub returned an invalid immutable revision",
        ));
    }
    Ok(())
}

fn ensure_cargo_available() -> MedusaResult<()> {
    Command::new("cargo")
        .arg("--version")
        .status()
        .map_err(command_error)
        .and_then(|status| {
            status.success().then_some(()).ok_or_else(|| {
                MedusaError::new(
                    ErrorCode::DependencyUnavailable,
                    ErrorCategory::Environment,
                    "cargo is required to update from Medusa main",
                )
            })
        })
}

#[cfg(any(windows, test))]
fn windows_source_script(
    parent_pid: u32,
    executable: &Path,
    script: &Path,
    revision: &str,
) -> String {
    let executable = powershell_quote(executable);
    let script = powershell_quote(script);
    format!(
        "$parentPid = {parent_pid}\nwhile (Get-Process -Id $parentPid -ErrorAction SilentlyContinue) {{ Start-Sleep -Milliseconds 200 }}\nGet-Process -Name medusa -ErrorAction SilentlyContinue | Where-Object {{ $_.Path -eq {executable} }} | Stop-Process -Force\nwhile (Get-Process -Name medusa -ErrorAction SilentlyContinue | Where-Object {{ $_.Path -eq {executable} }}) {{ Start-Sleep -Milliseconds 200 }}\n& cargo install --git '{REPOSITORY_URL}' --rev {revision} --locked --force --bin medusa medusa-cli\nif ($LASTEXITCODE -eq 0) {{ Start-Process -FilePath {executable} -ArgumentList '--fresh' }}\nRemove-Item -LiteralPath {script} -Force\n"
    )
}

#[cfg(any(not(windows), test))]
fn unix_source_script(parent_pid: u32, executable: &Path, script: &Path, revision: &str) -> String {
    format!(
        "#!/bin/sh\nwhile kill -0 {parent_pid} 2>/dev/null; do sleep 1; done\ncargo install --git '{REPOSITORY_URL}' --rev {revision} --locked --force --bin medusa medusa-cli && exec '{}' --fresh\nrm -f '{}'\n",
        shell_quote(executable),
        shell_quote(script),
    )
}

#[cfg(any(windows, test))]
fn powershell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}

#[cfg(any(not(windows), test))]
fn shell_quote(path: &Path) -> String {
    path.display().to_string().replace('\'', "'\\''")
}

fn http_error(error: impl std::fmt::Display) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Transient,
        format!("GitHub main branch request failed: {error}"),
    )
}

fn command_error(error: impl std::fmt::Display) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Environment,
        format!("could not start the main-branch updater: {error}"),
    )
}

fn internal_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        message,
    )
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use super::*;

    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn discovers_main_revision() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).expect("read request");
            let body = format!("{{\"sha\":\"{REVISION}\"}}");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body,
            )
            .expect("response");
        });
        assert_eq!(
            MainBranchUpdater::new(base)
                .expect("client")
                .latest_main()
                .expect("revision")
                .sha,
            REVISION
        );
        worker.join().expect("server");
    }

    #[test]
    fn rejects_invalid_main_revision() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).expect("read request");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 24\r\nConnection: close\r\n\r\n{\"sha\":\"main; echo pwn\"}")
                .expect("response");
        });
        assert!(
            MainBranchUpdater::new(base)
                .expect("client")
                .latest_main()
                .is_err()
        );
        worker.join().expect("server");
    }

    #[test]
    fn helpers_build_only_the_verified_revision() {
        let windows = windows_source_script(
            4242,
            Path::new(r"C:\bin\medusa.exe"),
            Path::new(r"C:\bin\medusa.main-update.ps1"),
            REVISION,
        );
        assert!(windows.contains("Get-Process -Id $parentPid"));
        assert!(windows.contains("Stop-Process -Force"));
        assert!(windows.contains(&format!("--rev {REVISION} --locked --force")));
        assert!(!windows.contains("--branch main"));
        assert!(
            windows
                .contains(r"Start-Process -FilePath 'C:\bin\medusa.exe' -ArgumentList '--fresh'")
        );

        let unix = unix_source_script(
            4242,
            Path::new("/usr/local/bin/medusa"),
            Path::new("/usr/local/bin/medusa.main-update.sh"),
            REVISION,
        );
        assert!(unix.contains(&format!("--rev {REVISION} --locked --force")));
        assert!(!unix.contains("--branch main"));
        assert!(unix.contains("exec '/usr/local/bin/medusa' --fresh"));
    }
}
