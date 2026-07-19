use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

#[cfg(unix)]
use std::process::Stdio;

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use time::OffsetDateTime;
use ulid::Ulid;

use crate::{
    paths::DaemonPaths,
    protocol::{
        DAEMON_PROTOCOL_VERSION, JobRecord, JobState, Request, RequestEnvelope, Response,
        ResponseEnvelope,
    },
    scheduler::{DaemonLimits, JobRunner, JobScheduler, SubmitError},
    transport::{LocalListener, LocalStream, connect, wake},
};

const MAX_REQUEST_BYTES: usize = 64 * 1024;
const REQUEST_IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Handle used to request graceful daemon shutdown from tests or embedding code.
pub struct ServerHandle {
    shutdown: Arc<AtomicBool>,
    socket: PathBuf,
}

impl ServerHandle {
    /// Stops accepting requests, wakes the listener, and lets accepted jobs drain.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = wake(&self.socket);
    }
}

/// Local IPC client. Every request uses a new connection, so reconnect is automatic.
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
        let mut stream = connect(&self.socket).map_err(transport_error)?;
        stream
            .set_read_timeout(Some(REQUEST_IO_TIMEOUT))
            .map_err(transport_error)?;
        stream
            .set_write_timeout(Some(REQUEST_IO_TIMEOUT))
            .map_err(transport_error)?;
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

/// Starts a daemon loop with production limits and blocks until graceful shutdown.
pub fn serve(paths: DaemonPaths) -> MedusaResult<()> {
    serve_with_limits(paths, DaemonLimits::default())
}

/// Starts a daemon loop with explicit worker and queue limits.
pub fn serve_with_limits(paths: DaemonPaths, limits: DaemonLimits) -> MedusaResult<()> {
    fs::create_dir_all(&paths.directory)?;
    let _ownership = Ownership::acquire(&paths)?;
    let (jobs, recovered) = load_and_recover(&paths)?;
    if recovered {
        persist_jobs(&paths, &jobs)?;
    }
    let jobs = Arc::new(Mutex::new(jobs));
    let listener = LocalListener::bind(&paths.socket).map_err(transport_error)?;
    let scheduler = match start_scheduler(&paths, &jobs, limits) {
        Ok(scheduler) => scheduler,
        Err(error) => {
            listener.cleanup();
            return Err(error);
        }
    };
    run_loop(
        listener,
        paths,
        jobs,
        Arc::new(AtomicBool::new(false)),
        scheduler,
    )
}

/// Starts the server in a dedicated thread with production limits.
pub fn spawn(
    paths: DaemonPaths,
) -> MedusaResult<(ServerHandle, thread::JoinHandle<MedusaResult<()>>)> {
    spawn_with_limits(paths, DaemonLimits::default())
}

/// Starts the server in a dedicated thread with explicit worker and queue limits.
pub fn spawn_with_limits(
    paths: DaemonPaths,
    limits: DaemonLimits,
) -> MedusaResult<(ServerHandle, thread::JoinHandle<MedusaResult<()>>)> {
    fs::create_dir_all(&paths.directory)?;
    limits.validate()?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let server_shutdown = Arc::clone(&shutdown);
    let socket = paths.socket.clone();
    let handle = thread::Builder::new()
        .name("medusa-daemon".to_owned())
        .spawn(move || {
            let _ownership = Ownership::acquire(&paths)?;
            let (jobs, recovered) = load_and_recover(&paths)?;
            if recovered {
                persist_jobs(&paths, &jobs)?;
            }
            let jobs = Arc::new(Mutex::new(jobs));
            let listener = LocalListener::bind(&paths.socket).map_err(transport_error)?;
            let scheduler = match start_scheduler(&paths, &jobs, limits) {
                Ok(scheduler) => scheduler,
                Err(error) => {
                    listener.cleanup();
                    return Err(error);
                }
            };
            run_loop(listener, paths, jobs, server_shutdown, scheduler)
        })
        .map_err(|error| {
            MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Environment,
                format!("failed to spawn daemon thread: {error}"),
            )
        })?;
    Ok((ServerHandle { shutdown, socket }, handle))
}

fn start_scheduler(
    paths: &DaemonPaths,
    jobs: &Arc<Mutex<BTreeMap<String, JobRecord>>>,
    limits: DaemonLimits,
) -> MedusaResult<JobScheduler> {
    let worker_paths = paths.clone();
    let worker_jobs = Arc::clone(jobs);
    let runner: JobRunner = Arc::new(move |job_id| {
        run_job(&worker_paths, &worker_jobs, &job_id);
    });
    JobScheduler::start(limits, runner)
}

fn run_loop(
    listener: LocalListener,
    paths: DaemonPaths,
    jobs: Arc<Mutex<BTreeMap<String, JobRecord>>>,
    shutdown: Arc<AtomicBool>,
    mut scheduler: JobScheduler,
) -> MedusaResult<()> {
    let result = (|| {
        while !shutdown.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok(stream) => {
                    let _ = handle_connection(stream, &paths, &jobs, &shutdown, &scheduler);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(transport_error(error)),
            }
        }
        Ok(())
    })();
    let scheduler_result = scheduler.shutdown();
    listener.cleanup();
    match (result, scheduler_result) {
        (Err(error), _) | (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn handle_connection(
    mut stream: LocalStream,
    paths: &DaemonPaths,
    jobs: &Arc<Mutex<BTreeMap<String, JobRecord>>>,
    shutdown: &Arc<AtomicBool>,
    scheduler: &JobScheduler,
) -> MedusaResult<()> {
    stream
        .set_read_timeout(Some(REQUEST_IO_TIMEOUT))
        .map_err(transport_error)?;
    stream
        .set_write_timeout(Some(REQUEST_IO_TIMEOUT))
        .map_err(transport_error)?;
    let reader_stream = stream.try_clone().map_err(transport_error)?;
    let mut reader = BufReader::new(reader_stream).take((MAX_REQUEST_BYTES + 1) as u64);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if line.trim().is_empty() {
        return Ok(());
    }
    if line.len() > MAX_REQUEST_BYTES {
        return write_response(
            &mut stream,
            Response::Error {
                code: "request_too_large".into(),
                message: format!("daemon request exceeds {MAX_REQUEST_BYTES} bytes"),
            },
        );
    }
    let envelope: RequestEnvelope = serde_json::from_str(&line)?;
    let response = if envelope.version != DAEMON_PROTOCOL_VERSION {
        Response::Error {
            code: "incompatible_protocol".into(),
            message: format!("unsupported protocol {}", envelope.version),
        }
    } else {
        dispatch(envelope.request, paths, jobs, shutdown, scheduler)?
    };
    write_response(&mut stream, response)
}

fn write_response(stream: &mut LocalStream, response: Response) -> MedusaResult<()> {
    serde_json::to_writer(
        &mut *stream,
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
    scheduler: &JobScheduler,
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
            match scheduler.enqueue(job.id.clone()) {
                Ok(()) => Ok(Response::Submitted { job }),
                Err(SubmitError::Busy) => {
                    discard_rejected_job(paths, jobs, &job.id)?;
                    Ok(Response::Error {
                        code: "daemon_busy".into(),
                        message: "daemon job queue is at capacity; retry later".into(),
                    })
                }
                Err(SubmitError::Stopped) => {
                    discard_rejected_job(paths, jobs, &job.id)?;
                    Ok(Response::Error {
                        code: "daemon_stopping".into(),
                        message: "daemon is shutting down and no longer accepts jobs".into(),
                    })
                }
            }
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

fn discard_rejected_job(
    paths: &DaemonPaths,
    jobs: &Arc<Mutex<BTreeMap<String, JobRecord>>>,
    job_id: &str,
) -> MedusaResult<()> {
    let mut locked = lock_jobs(jobs)?;
    locked.remove(job_id);
    persist_jobs(paths, &locked)
}

fn run_job(
    paths: &DaemonPaths,
    jobs: &Arc<Mutex<BTreeMap<String, JobRecord>>>,
    job_id: &str,
) {
    if let Err(error) = run_job_inner(paths, jobs, job_id) {
        mark_job_failed(paths, jobs, job_id, format!("daemon worker failed: {error}"));
    }
}

fn run_job_inner(
    paths: &DaemonPaths,
    jobs: &Arc<Mutex<BTreeMap<String, JobRecord>>>,
    job_id: &str,
) -> MedusaResult<()> {
    let command = {
        let mut locked = lock_jobs(jobs)?;
        let Some(job) = locked.get_mut(job_id) else {
            return Err(MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                format!("daemon queued job disappeared before execution: {job_id}"),
            ));
        };
        job.state = JobState::Running;
        job.started_at = Some(OffsetDateTime::now_utc());
        let command = (job.program.clone(), job.args.clone());
        persist_jobs(paths, &locked)?;
        command
    };

    let output = Command::new(&command.0)
        .args(&command.1)
        .current_dir(&paths.repo)
        .output();
    let mut locked = lock_jobs(jobs)?;
    let Some(job) = locked.get_mut(job_id) else {
        return Err(MedusaError::new(
            ErrorCode::InternalInvariant,
            ErrorCategory::Internal,
            format!("daemon running job disappeared before completion: {job_id}"),
        ));
    };
    job.finished_at = Some(OffsetDateTime::now_utc());
    match output {
        Ok(output) => {
            job.exit_code = output.status.code();
            job.stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            job.stderr = String::from_utf8_lossy(&output.stderr).into_owned();
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
    persist_jobs(paths, &locked)
}

fn mark_job_failed(
    paths: &DaemonPaths,
    jobs: &Arc<Mutex<BTreeMap<String, JobRecord>>>,
    job_id: &str,
    message: String,
) {
    let Ok(mut locked) = lock_jobs(jobs) else {
        return;
    };
    let Some(job) = locked.get_mut(job_id) else {
        return;
    };
    job.state = JobState::Failed;
    job.finished_at = Some(OffsetDateTime::now_utc());
    job.stderr = message;
    let _ = persist_jobs(paths, &locked);
}

fn load_and_recover(paths: &DaemonPaths) -> MedusaResult<(BTreeMap<String, JobRecord>, bool)> {
    fs::create_dir_all(&paths.directory)?;
    restore_backup_if_needed(paths)?;
    if !paths.state.exists() {
        return Ok((BTreeMap::new(), false));
    }
    let mut jobs: BTreeMap<String, JobRecord> = serde_json::from_slice(&fs::read(&paths.state)?)?;
    let mut recovered = false;
    for job in jobs.values_mut() {
        if matches!(job.state, JobState::Queued | JobState::Running) {
            job.state = JobState::Interrupted;
            job.finished_at = Some(OffsetDateTime::now_utc());
            job.stderr
                .push_str("\n[daemon restarted before process completion]");
            recovered = true;
        }
    }
    Ok((jobs, recovered))
}

fn persist_jobs(paths: &DaemonPaths, jobs: &BTreeMap<String, JobRecord>) -> MedusaResult<()> {
    fs::create_dir_all(&paths.directory)?;
    let temporary = paths.state.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(jobs)?)?;
    replace_file(&temporary, &paths.state)?;
    Ok(())
}

#[cfg(unix)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    let backup = backup_path(destination);
    if backup.exists() {
        fs::remove_file(&backup)?;
    }
    if destination.exists() {
        fs::rename(destination, &backup)?;
    }
    match fs::rename(source, destination) {
        Ok(()) => {
            let _ = fs::remove_file(backup);
            Ok(())
        }
        Err(error) => {
            if backup.exists() {
                let _ = fs::rename(backup, destination);
            }
            Err(error)
        }
    }
}

fn restore_backup_if_needed(paths: &DaemonPaths) -> std::io::Result<()> {
    let backup = backup_path(&paths.state);
    if !paths.state.exists() && backup.exists() {
        fs::rename(backup, &paths.state)?;
    }
    Ok(())
}

fn backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
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

fn transport_error(error: impl std::fmt::Display) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Environment,
        format!("daemon transport error: {error}"),
    )
}

struct Ownership {
    path: PathBuf,
    _file: File,
}

impl Ownership {
    fn acquire(paths: &DaemonPaths) -> MedusaResult<Self> {
        fs::create_dir_all(&paths.directory)?;
        if paths.owner.exists() {
            if owner_process_alive(&paths.owner) {
                return Err(MedusaError::new(
                    ErrorCode::PolicyDenied,
                    ErrorCategory::Policy,
                    "daemon ownership unavailable: the recorded owner process is still running",
                ));
            }
            let _ = fs::remove_file(&paths.owner);
            let _ = fs::remove_file(&paths.socket);
        }
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

fn owner_process_alive(owner: &Path) -> bool {
    let Ok(raw) = fs::read_to_string(owner) else {
        return false;
    };
    let Ok(pid) = raw.trim().parse::<u32>() else {
        return false;
    };
    process_is_alive(pid)
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
mod tests;
