use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

use crate::{DaemonClient, DaemonPaths, Request, Response};

// A cold Windows process can take several seconds to rebuild daemon state and
// publish its loopback endpoint. Keep lifecycle status accurate instead of
// reporting a transient degraded state while that process is still starting.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const RESTART_BACKOFF: Duration = Duration::from_secs(2);
const READY_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Command used by a frontend to host the repository daemon in a detached process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonLaunch {
    executable: PathBuf,
    arguments: Vec<OsString>,
}

impl DaemonLaunch {
    #[must_use]
    pub fn new(
        executable: impl Into<PathBuf>,
        arguments: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        Self {
            executable: executable.into(),
            arguments: arguments.into_iter().map(Into::into).collect(),
        }
    }

    pub fn for_current_executable() -> MedusaResult<Self> {
        let executable = std::env::current_exe().map_err(|error| {
            MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Environment,
                format!("cannot locate the current Medusa executable: {error}"),
            )
        })?;
        Ok(Self::new(executable, ["__daemon-serve"]))
    }

    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    fn spawn(&self, paths: &DaemonPaths) -> MedusaResult<()> {
        let mut command = Command::new(&self.executable);
        command
            .args(&self.arguments)
            .arg("--repo")
            .arg(&paths.repo)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_detached(&mut command);
        command.spawn().map(|_| ()).map_err(|error| {
            MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Environment,
                format!(
                    "failed to launch daemon host {}: {error}",
                    self.executable.display()
                ),
            )
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DaemonLifecycleState {
    Connected,
    Started,
    Recovered,
    Degraded,
}

impl DaemonLifecycleState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Started => "started",
            Self::Recovered => "recovered",
            Self::Degraded => "degraded",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonLifecycle {
    pub state: DaemonLifecycleState,
    pub detail: String,
}

impl DaemonLifecycle {
    fn connected(detail: impl Into<String>) -> Self {
        Self {
            state: DaemonLifecycleState::Connected,
            detail: detail.into(),
        }
    }

    fn degraded(detail: impl Into<String>) -> Self {
        Self {
            state: DaemonLifecycleState::Degraded,
            detail: detail.into(),
        }
    }
}

type Launcher = Arc<dyn Fn(&DaemonPaths) -> MedusaResult<()> + Send + Sync + 'static>;

/// Repository-scoped lifecycle owner shared by terminal and desktop frontends.
pub struct DaemonSupervisor {
    paths: DaemonPaths,
    launcher: Option<Launcher>,
    next_retry: Instant,
}

impl DaemonSupervisor {
    #[must_use]
    pub fn new(repo: &Path, launch: DaemonLaunch) -> Self {
        let launcher: Launcher = Arc::new(move |paths| launch.spawn(paths));
        Self::with_optional_launcher(repo, Some(launcher))
    }

    #[must_use]
    pub fn observe_only(repo: &Path) -> Self {
        Self::with_optional_launcher(repo, None)
    }

    #[must_use]
    pub fn paths(&self) -> &DaemonPaths {
        &self.paths
    }

    #[must_use]
    pub fn client(&self) -> DaemonClient {
        DaemonClient::new(&self.paths.socket)
    }

    /// Observes the daemon and performs a bounded restart attempt when needed.
    pub fn poll(&mut self) -> DaemonLifecycle {
        if daemon_ready(&self.paths) {
            self.next_retry = Instant::now();
            return DaemonLifecycle::connected("daemon is ready");
        }
        if self.launcher.is_none() {
            return DaemonLifecycle::degraded(
                "daemon is unavailable and this frontend has no launch command",
            );
        }
        let now = Instant::now();
        if now < self.next_retry {
            return DaemonLifecycle::degraded("daemon restart is waiting for the retry backoff");
        }
        match self.ensure_running() {
            Ok(lifecycle) => lifecycle,
            Err(error) => {
                self.next_retry = Instant::now() + RESTART_BACKOFF;
                DaemonLifecycle::degraded(format!("daemon restart failed: {error}"))
            }
        }
    }

    /// Ensures that exactly one external daemon owns this repository and waits for readiness.
    pub fn ensure_running(&mut self) -> MedusaResult<DaemonLifecycle> {
        if daemon_ready(&self.paths) {
            self.next_retry = Instant::now();
            return Ok(DaemonLifecycle::connected("daemon is already running"));
        }
        let launcher = self.launcher.as_ref().ok_or_else(|| {
            MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Environment,
                "daemon is unavailable and no launch command is configured",
            )
        })?;
        fs::create_dir_all(&self.paths.directory)?;
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        let had_previous_instance = self.paths.socket.exists() || self.paths.owner.exists();

        loop {
            if daemon_ready(&self.paths) {
                self.next_retry = Instant::now();
                return Ok(DaemonLifecycle::connected(
                    "daemon became ready while another frontend was starting it",
                ));
            }
            match StartupLock::try_acquire(&self.paths.startup)? {
                StartupLockAttempt::Acquired(_lock) => {
                    if daemon_ready(&self.paths) {
                        self.next_retry = Instant::now();
                        return Ok(DaemonLifecycle::connected(
                            "daemon became ready before launch",
                        ));
                    }
                    launcher(&self.paths)?;
                    wait_for_ready(&self.paths, deadline)?;
                    self.next_retry = Instant::now();
                    return Ok(DaemonLifecycle {
                        state: if had_previous_instance {
                            DaemonLifecycleState::Recovered
                        } else {
                            DaemonLifecycleState::Started
                        },
                        detail: if had_previous_instance {
                            "daemon recovered after a disconnected or stale instance".to_owned()
                        } else {
                            "daemon started by the frontend lifecycle supervisor".to_owned()
                        },
                    });
                }
                StartupLockAttempt::Busy { owner_pid } => {
                    let reclaim = match owner_pid {
                        Some(pid) => !process_is_alive(pid),
                        None => startup_lock_is_stale(&self.paths.startup),
                    };
                    if reclaim {
                        let _ = fs::remove_file(&self.paths.startup);
                        continue;
                    }
                    if Instant::now() >= deadline {
                        return Err(lifecycle_error(
                            "another frontend owns daemon startup but readiness timed out",
                        ));
                    }
                    thread::sleep(READY_POLL_INTERVAL);
                }
            }
        }
    }

    /// Requests graceful shutdown. Accepted jobs remain owned by the daemon and drain first.
    pub fn shutdown(&mut self) -> MedusaResult<DaemonLifecycle> {
        match self.client().request(Request::Shutdown)? {
            Response::Ack => {
                let deadline = Instant::now() + STARTUP_TIMEOUT;
                while Instant::now() < deadline {
                    if !daemon_ready(&self.paths) {
                        self.next_retry = Instant::now() + RESTART_BACKOFF;
                        return Ok(DaemonLifecycle::degraded(
                            "daemon stopped after draining accepted jobs",
                        ));
                    }
                    thread::sleep(READY_POLL_INTERVAL);
                }
                Ok(DaemonLifecycle::degraded(
                    "daemon accepted shutdown and is still draining running jobs",
                ))
            }
            response => Err(lifecycle_error(format!(
                "daemon returned an unexpected shutdown response: {response:?}"
            ))),
        }
    }

    fn with_optional_launcher(repo: &Path, launcher: Option<Launcher>) -> Self {
        Self {
            paths: DaemonPaths::for_repo(repo),
            launcher,
            next_retry: Instant::now(),
        }
    }

    #[cfg(test)]
    fn with_launcher(repo: &Path, launcher: Launcher) -> Self {
        Self::with_optional_launcher(repo, Some(launcher))
    }
}

fn daemon_ready(paths: &DaemonPaths) -> bool {
    matches!(
        DaemonClient::new(&paths.socket).request(Request::Ping),
        Ok(Response::Pong)
    )
}

fn wait_for_ready(paths: &DaemonPaths, deadline: Instant) -> MedusaResult<()> {
    while Instant::now() < deadline {
        if daemon_ready(paths) {
            return Ok(());
        }
        thread::sleep(READY_POLL_INTERVAL);
    }
    Err(lifecycle_error(format!(
        "daemon endpoint {} did not become ready within {} seconds",
        paths.socket.display(),
        STARTUP_TIMEOUT.as_secs()
    )))
}

fn startup_lock_is_stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age >= STARTUP_TIMEOUT)
}

fn lifecycle_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Environment,
        message,
    )
}

enum StartupLockAttempt {
    Acquired(StartupLock),
    Busy { owner_pid: Option<u32> },
}

struct StartupLock {
    path: PathBuf,
    _file: File,
}

impl StartupLock {
    fn try_acquire(path: &Path) -> MedusaResult<StartupLockAttempt> {
        match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(mut file) => {
                writeln!(file, "{}", std::process::id())?;
                file.flush()?;
                Ok(StartupLockAttempt::Acquired(Self {
                    path: path.to_path_buf(),
                    _file: file,
                }))
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let owner_pid = fs::read_to_string(path)
                    .ok()
                    .and_then(|raw| raw.trim().parse::<u32>().ok());
                Ok(StartupLockAttempt::Busy { owner_pid })
            }
            Err(error) => Err(lifecycle_error(format!(
                "cannot acquire daemon startup lock {}: {error}",
                path.display()
            ))),
        }
    }
}

impl Drop for StartupLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
fn configure_detached(command: &mut Command) {
    command.process_group(0);
}

#[cfg(windows)]
fn configure_detached(command: &mut Command) {
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    let pid = pid.to_string();
    Command::new("kill")
        .args(["-0", pid.as_str()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    let filter = format!("PID eq {pid}");
    Command::new("tasklist")
        .args(["/FI", filter.as_str(), "/FO", "CSV", "/NH"])
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
        })
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc, Barrier, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
    };

    use super::*;
    use crate::{ServerHandle, spawn};

    type ServerRecord = (ServerHandle, thread::JoinHandle<MedusaResult<()>>);

    fn test_launcher(
        launches: Arc<AtomicUsize>,
        servers: Arc<Mutex<Vec<ServerRecord>>>,
    ) -> Launcher {
        Arc::new(move |paths| {
            launches.fetch_add(1, Ordering::SeqCst);
            let server = spawn(paths.clone())?;
            servers
                .lock()
                .map_err(|_| lifecycle_error("test server registry is poisoned"))?
                .push(server);
            Ok(())
        })
    }

    fn stop_servers(servers: &Arc<Mutex<Vec<ServerRecord>>>) {
        let records = servers
            .lock()
            .expect("server registry")
            .drain(..)
            .collect::<Vec<_>>();
        for (handle, server) in records {
            handle.shutdown();
            server.join().expect("join daemon").expect("daemon result");
        }
    }

    #[test]
    fn concurrent_frontends_launch_exactly_one_daemon() {
        let directory = tempfile::tempdir().expect("tempdir");
        let launches = Arc::new(AtomicUsize::new(0));
        let servers = Arc::new(Mutex::new(Vec::new()));
        let launcher = test_launcher(Arc::clone(&launches), Arc::clone(&servers));
        let barrier = Arc::new(Barrier::new(9));
        let workers = (0..8)
            .map(|_| {
                let repo = directory.path().to_path_buf();
                let launcher = Arc::clone(&launcher);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let mut supervisor = DaemonSupervisor::with_launcher(&repo, launcher);
                    barrier.wait();
                    supervisor.ensure_running()
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        for worker in workers {
            let lifecycle = worker.join().expect("join frontend").expect("lifecycle");
            assert_ne!(lifecycle.state, DaemonLifecycleState::Degraded);
        }
        assert_eq!(launches.load(Ordering::SeqCst), 1);
        stop_servers(&servers);
    }

    #[test]
    fn fresh_empty_startup_lock_is_not_reclaimed() {
        let directory = tempfile::tempdir().expect("tempdir");
        let paths = DaemonPaths::for_repo(directory.path());
        fs::create_dir_all(&paths.directory).expect("daemon directory");
        fs::write(&paths.startup, []).expect("empty startup lock");
        assert!(!startup_lock_is_stale(&paths.startup));
    }

    #[test]
    fn disconnected_daemon_is_restarted_after_backoff() {
        let directory = tempfile::tempdir().expect("tempdir");
        let launches = Arc::new(AtomicUsize::new(0));
        let servers = Arc::new(Mutex::new(Vec::new()));
        let launcher = test_launcher(Arc::clone(&launches), Arc::clone(&servers));
        let mut supervisor = DaemonSupervisor::with_launcher(directory.path(), launcher);

        assert_eq!(
            supervisor.ensure_running().expect("initial start").state,
            DaemonLifecycleState::Started
        );
        supervisor.shutdown().expect("shutdown");
        thread::sleep(RESTART_BACKOFF);
        let lifecycle = supervisor.poll();
        assert!(matches!(
            lifecycle.state,
            DaemonLifecycleState::Started | DaemonLifecycleState::Recovered
        ));
        assert_eq!(launches.load(Ordering::SeqCst), 2);
        stop_servers(&servers);
    }
}
