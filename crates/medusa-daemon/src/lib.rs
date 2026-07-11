//! Persistent local daemon, socket protocol, process ownership, and crash recovery.

#![cfg(unix)]

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use ulid::Ulid;

/// Version of the daemon wire protocol.
pub const DAEMON_PROTOCOL_VERSION: u16 = 1;

/// Durable job lifecycle.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Interrupted,
}

/// One durable daemon-owned process record.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JobRecord {
    pub id: String,
    pub program: String,
    pub args: Vec<String>,
    pub state: JobState,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Client request envelope.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RequestEnvelope {
    pub version: u16,
    pub request: Request,
}

/// Supported daemon requests.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Ping,
    Submit { program: String, args: Vec<String> },
    Status { job_id: String },
    List,
    Shutdown,
}

/// Server response envelope.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResponseEnvelope {
    pub version: u16,
    pub response: Response,
}

/// Supported daemon responses.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Pong,
    Submitted { job: JobRecord },
    Status { job: Option<JobRecord> },
    Jobs { jobs: Vec<JobRecord> },
    Ack,
    Error { code: String, message: String },
}

/// Filesystem layout for one daemon instance.
#[derive(Clone, Debug)]
pub struct DaemonPaths {
    pub repo: PathBuf,
    pub directory: PathBuf,
    pub socket: PathBuf,
    pub state: PathBuf,
    pub owner: PathBuf,
}

impl DaemonPaths {
    #[must_use]
    pub fn for_repo(repo: &Path) -> Self {
        let directory = repo.join(".medusa/daemon");
        Self {
            repo: repo.to_path_buf(),
            socket: directory.join("medusa.sock"),
            state: directory.join("jobs.json"),
            owner: directory.join("owner.pid"),
            directory,
        }
    }
}

/// Handle used to request daemon shutdown from tests or embedding code.
pub struct ServerHandle {
    shutdown: Arc<AtomicBool>,
    socket: PathBuf,
}

impl ServerHandle {
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&self.socket);
    }
}

/// Local socket client. Every request uses a new connection, so reconnect is automatic.
#[derive(Clone, Debug)]
pub struct DaemonClient {
    socket: PathBuf,
}

impl DaemonClient {
    #[must_use]
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    pub fn request(&self, request: Request) -> MedusaResult<Response> {
        let mut stream = UnixStream::connect(&self.socket).map_err(socket_error)?;
        let envelope = RequestEnvelope {
            version: DAEMON_PROTOCOL_VERSION,
            request,
        };
        serde_json::to_writer(&mut stream, &envelope)?;
        stream.write_all(b"\n")?;
        stream.flush()?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line)?;
        let response: ResponseEnvelope = serde_json::from_str(&line)?;
        if response.version != DAEMON_PROTOCOL_VERSION {
            return Err(MedusaError::new(
                ErrorCode::IncompatibleProtocol,
                ErrorCategory::Validation,
                format!("daemon protocol {} is unsupported", response.version),
            ));
        }
        Ok(response.response)
    }
}

/// Starts a daemon loop and blocks until shutdown.
pub fn serve(paths: DaemonPaths) -> MedusaResult<()> {
    let _ownership = Ownership::acquire(&paths)?;
    let (jobs, recovered) = load_and_recover(&paths)?;
    if recovered {
        persist_jobs(&paths, &jobs)?;
    }
    if paths.socket.exists() {
        fs::remove_file(&paths.socket)?;
    }
    let listener = UnixListener::bind(&paths.socket).map_err(socket_error)?;
    listener.set_nonblocking(true).map_err(socket_error)?;
    let jobs = Arc::new(Mutex::new(jobs));
    let shutdown = Arc::new(AtomicBool::new(false));
    run_loop(listener, paths, jobs, shutdown)
}

/// Starts the server in a dedicated thread.
pub fn spawn(paths: DaemonPaths) -> MedusaResult<(ServerHandle, thread::JoinHandle<MedusaResult<()>>)> {
    fs::create_dir_all(&paths.directory)?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let server_shutdown = shutdown.clone();
    let socket = paths.socket.clone();
    let handle = thread::spawn(move || {
        let _ownership = Ownership::acquire(&paths)?;
        let (jobs, recovered) = load_and_recover(&paths)?;
        if recovered {
            persist_jobs(&paths, &jobs)?;
        }
        if paths.socket.exists() {
            fs::remove_file(&paths.socket)?;
        }
        let listener = UnixListener::bind(&paths.socket).map_err(socket_error)?;
        listener.set_nonblocking(true).map_err(socket_error)?;
        run_loop(listener, paths, Arc::new(Mutex::new(jobs)), server_shutdown)
    });
    Ok((ServerHandle { shutdown, socket }, handle))
}

fn run_loop(
    listener: UnixListener,
    paths: DaemonPaths,
    jobs: Arc<Mutex<BTreeMap<String, JobRecord>>>,
    shutdown: Arc<AtomicBool>,
) -> MedusaResult<()> {
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                handle_connection(stream, &paths, &jobs, &shutdown)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(socket_error(error)),
        }
    }
    let _ = fs::remove_file(&paths.socket);
    Ok(())
}

fn handle_connection(
    mut stream: UnixStream,
    paths: &DaemonPaths,
    jobs: &Arc<Mutex<BTreeMap<String, JobRecord>>>,
    shutdown: &Arc<AtomicBool>,
) -> MedusaResult<()> {
    let mut line = String::new();
    BufReader::new(stream.try_clone().map_err(socket_error)?).read_line(&mut line)?;
    if line.trim().is_empty() {
        return Ok(());
    }
    let envelope: RequestEnvelope = serde_json::from_str(&line)?;
    let response = if envelope.version != DAEMON_PROTOCOL_VERSION {
        Response::Error {
            code: "incompatible_protocol".into(),
            message: format!("unsupported protocol {}", envelope.version),
        }
    } else {
        dispatch(envelope.request, paths, jobs, shutdown)?
    };
    serde_json::to_writer(
        &mut stream,
        &ResponseEnvelope {
            version: DAEMON_PROTOCOL_VERSION,
            response,
        },
    )?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

fn dispatch(
    request: Request,
    paths: &DaemonPaths,
    jobs: &Arc<Mutex<BTreeMap<String, JobRecord>>>,
    shutdown: &Arc<AtomicBool>,
) -> MedusaResult<Response> {
    match request {
        Request::Ping => Ok(Response::Pong),
        Request::Submit { program, args } => {
            validate_program(&program)?;
            let now = OffsetDateTime::now_utc();
            let job = JobRecord {
                id: format!("job-{}", Ulid::new()),
                program,
                args,
                state: JobState::Queued,
                created_at: now,
                started_at: None,
                finished_at: None,
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
            };
            {
                let mut locked = lock_jobs(jobs)?;
                locked.insert(job.id.clone(), job.clone());
                persist_jobs(paths, &locked)?;
            }
            spawn_job(paths.clone(), jobs.clone(), job.id.clone());
            Ok(Response::Submitted { job })
        }
        Request::Status { job_id } => {
            let locked = lock_jobs(jobs)?;
            Ok(Response::Status {
                job: locked.get(&job_id).cloned(),
            })
        }
        Request::List => {
            let locked = lock_jobs(jobs)?;
            Ok(Response::Jobs {
                jobs: locked.values().cloned().collect(),
            })
        }
        Request::Shutdown => {
            shutdown.store(true, Ordering::SeqCst);
            Ok(Response::Ack)
        }
    }
}

fn spawn_job(paths: DaemonPaths, jobs: Arc<Mutex<BTreeMap<String, JobRecord>>>, job_id: String) {
    thread::spawn(move || {
        let command = {
            let mut locked = match lock_jobs(&jobs) {
                Ok(locked) => locked,
                Err(_) => return,
            };
            let Some(job) = locked.get_mut(&job_id) else {
                return;
            };
            job.state = JobState::Running;
            job.started_at = Some(OffsetDateTime::now_utc());
            let command = (job.program.clone(), job.args.clone());
            let _ = persist_jobs(&paths, &locked);
            command
        };

        let output = Command::new(&command.0)
            .args(&command.1)
            .current_dir(&paths.repo)
            .output();
        let mut locked = match lock_jobs(&jobs) {
            Ok(locked) => locked,
            Err(_) => return,
        };
        let Some(job) = locked.get_mut(&job_id) else {
            return;
        };
        job.finished_at = Some(OffsetDateTime::now_utc());
        match output {
            Ok(output) => {
                job.exit_code = output.status.code();
                job.stdout = truncate(String::from_utf8_lossy(&output.stdout).into_owned());
                job.stderr = truncate(String::from_utf8_lossy(&output.stderr).into_owned());
                job.state = if output.status.success() {
                    JobState::Succeeded
                } else {
                    JobState::Failed
                };
            }
            Err(error) => {
                job.stderr = error.to_string();
                job.state = JobState::Failed;
            }
        }
        let _ = persist_jobs(&paths, &locked);
    });
}

fn load_and_recover(paths: &DaemonPaths) -> MedusaResult<(BTreeMap<String, JobRecord>, bool)> {
    fs::create_dir_all(&paths.directory)?;
    if !paths.state.exists() {
        return Ok((BTreeMap::new(), false));
    }
    let mut jobs: BTreeMap<String, JobRecord> = serde_json::from_slice(&fs::read(&paths.state)?)?;
    let mut recovered = false;
    for job in jobs.values_mut() {
        if matches!(job.state, JobState::Queued | JobState::Running) {
            job.state = JobState::Interrupted;
            job.finished_at = Some(OffsetDateTime::now_utc());
            job.stderr.push_str("\n[daemon restarted before process completion]");
            recovered = true;
        }
    }
    Ok((jobs, recovered))
}

fn persist_jobs(paths: &DaemonPaths, jobs: &BTreeMap<String, JobRecord>) -> MedusaResult<()> {
    fs::create_dir_all(&paths.directory)?;
    let temporary = paths.state.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(jobs)?)?;
    fs::rename(temporary, &paths.state)?;
    Ok(())
}

fn validate_program(program: &str) -> MedusaResult<()> {
    if program.is_empty() || matches!(program, "rm" | "sudo" | "shutdown" | "reboot" | "mkfs") {
        return Err(MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            format!("daemon denied program: {program}"),
        ));
    }
    Ok(())
}

fn lock_jobs(
    jobs: &Arc<Mutex<BTreeMap<String, JobRecord>>>,
) -> MedusaResult<std::sync::MutexGuard<'_, BTreeMap<String, JobRecord>>> {
    jobs.lock().map_err(|_| {
        MedusaError::new(
            ErrorCode::InternalInvariant,
            ErrorCategory::Internal,
            "daemon job state lock was poisoned",
        )
    })
}

fn truncate(mut value: String) -> String {
    const LIMIT: usize = 1_000_000;
    if value.len() > LIMIT {
        value.truncate(LIMIT);
        value.push_str("\n[truncated]");
    }
    value
}

fn socket_error(error: impl std::fmt::Display) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Environment,
        format!("daemon socket error: {error}"),
    )
}

struct Ownership {
    path: PathBuf,
    _file: File,
}

impl Ownership {
    fn acquire(paths: &DaemonPaths) -> MedusaResult<Self> {
        fs::create_dir_all(&paths.directory)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&paths.owner)
            .map_err(|error| {
                MedusaError::new(
                    ErrorCode::PolicyDenied,
                    ErrorCategory::Policy,
                    format!("daemon ownership unavailable: {error}"),
                )
            })?;
        writeln!(file, "{}", std::process::id())?;
        file.flush()?;
        Ok(Self {
            path: paths.owner.clone(),
            _file: file,
        })
    }
}

impl Drop for Ownership {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for_socket(path: &Path) {
        for _ in 0..100 {
            if path.exists() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("socket did not appear: {}", path.display());
    }

    #[test]
    fn client_reconnects_while_job_continues() {
        let directory = tempfile::tempdir().expect("tempdir");
        let paths = DaemonPaths::for_repo(directory.path());
        let (handle, server) = spawn(paths.clone()).expect("spawn daemon");
        wait_for_socket(&paths.socket);

        let first_client = DaemonClient::new(&paths.socket);
        let Response::Submitted { job } = first_client
            .request(Request::Submit {
                program: "sh".into(),
                args: vec![
                    "-c".into(),
                    "sleep 0.3; printf done > finished.txt; printf verified-daemon-reconnect".into(),
                ],
            })
            .expect("submit")
        else {
            panic!("unexpected submit response");
        };
        drop(first_client);

        let second_client = DaemonClient::new(&paths.socket);
        let completed = (0..100).find_map(|_| {
            let Response::Status { job: Some(current) } = second_client
                .request(Request::Status {
                    job_id: job.id.clone(),
                })
                .ok()?
            else {
                return None;
            };
            if matches!(current.state, JobState::Succeeded | JobState::Failed) {
                Some(current)
            } else {
                thread::sleep(Duration::from_millis(20));
                None
            }
        });
        let completed = completed.expect("job completion after reconnect");
        assert_eq!(completed.state, JobState::Succeeded);
        assert!(completed.stdout.contains("verified-daemon-reconnect"));
        assert_eq!(
            fs::read_to_string(directory.path().join("finished.txt")).expect("finished file"),
            "done"
        );

        handle.shutdown();
        server.join().expect("join daemon").expect("daemon result");
    }

    #[test]
    fn restart_marks_orphaned_jobs_interrupted() {
        let directory = tempfile::tempdir().expect("tempdir");
        let paths = DaemonPaths::for_repo(directory.path());
        fs::create_dir_all(&paths.directory).expect("daemon directory");
        let job = JobRecord {
            id: "job-test".into(),
            program: "sh".into(),
            args: vec![],
            state: JobState::Running,
            created_at: OffsetDateTime::now_utc(),
            started_at: Some(OffsetDateTime::now_utc()),
            finished_at: None,
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
        };
        persist_jobs(&paths, &BTreeMap::from([(job.id.clone(), job)])).expect("persist");
        let (jobs, recovered) = load_and_recover(&paths).expect("recover");
        assert!(recovered);
        assert_eq!(jobs["job-test"].state, JobState::Interrupted);
    }
}
