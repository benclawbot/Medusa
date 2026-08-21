use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread,
    time::Duration,
};

#[cfg(test)]
use std::sync::atomic::AtomicUsize;

#[cfg(unix)]
use std::process::Command;

#[cfg(unix)]
use std::process::Stdio;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use medusa_config::Config;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_protocol::frontend::FrontendCommandEnvelope;
use time::OffsetDateTime;
use ulid::Ulid;

use crate::{
    cancellation::{append_detail, cancel_all_jobs, cancel_job, mark_job_interrupted},
    frontend_control::{FrontendCommandAcknowledgement, FrontendControlPlane},
    paths::DaemonPaths,
    process::ProcessRegistry,
    protocol::{
        DAEMON_PROTOCOL_VERSION, FrontendArtifactUpload, FrontendCredentialUpdate, JobRecord,
        JobState, Request, RequestEnvelope, Response, ResponseEnvelope,
    },
    scheduler::{DaemonLimits, JobRunner, JobScheduler, SubmitError},
    transport::{LocalListener, LocalStream, connect, wake},
};

const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_ARTIFACT_REQUEST_BYTES: usize = 32 * 1024 * 1024;
const REQUEST_IO_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECTION_WORKERS: usize = 8;
const CONNECTION_QUEUE_CAPACITY: usize = 32;
const SHUTDOWN_NONE: u8 = 0;
const SHUTDOWN_GRACEFUL: u8 = 1;
const SHUTDOWN_IMMEDIATE: u8 = 2;

#[cfg(test)]
static FRONTEND_LOCK_PROBE_TARGET: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static FRONTEND_LOCK_PROBE_HITS: AtomicUsize = AtomicUsize::new(0);

/// Handle used to request daemon shutdown from tests or embedding code.
pub struct ServerHandle {
    shutdown: Arc<AtomicU8>,
    socket: PathBuf,
}

impl ServerHandle {
    /// Stops accepting requests, wakes the listener, and lets accepted jobs drain.
    pub fn shutdown(&self) {
        request_shutdown(&self.shutdown, SHUTDOWN_GRACEFUL);
        let _ = wake(&self.socket);
    }

    /// Stops accepting requests and cancels queued and running jobs before worker join.
    pub fn shutdown_now(&self) {
        request_shutdown(&self.shutdown, SHUTDOWN_IMMEDIATE);
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

    /// Sends one versioned frontend command through the repository-scoped daemon authority.
    pub fn frontend(
        &self,
        envelope: FrontendCommandEnvelope,
    ) -> MedusaResult<FrontendCommandAcknowledgement> {
        match self.request(Request::Frontend { envelope })? {
            Response::Frontend { acknowledgement } => Ok(acknowledgement),
            Response::Error { code, message } => Err(frontend_request_error(code, message)),
            response => Err(MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                format!("daemon returned an unexpected frontend response: {response:?}"),
            )),
        }
    }

    /// Stages one bounded attachment in the daemon-owned content-addressed artifact store.
    pub fn frontend_artifact(&self, upload: FrontendArtifactUpload) -> MedusaResult<String> {
        match self.request(Request::FrontendArtifact { upload })? {
            Response::FrontendArtifact { artifact_id } => Ok(artifact_id),
            Response::Error { code, message } => Err(frontend_request_error(code, message)),
            response => Err(MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                format!("daemon returned an unexpected artifact response: {response:?}"),
            )),
        }
    }

    /// Exports one verified bounded artifact for a remote frontend delivery adapter.
    pub fn frontend_artifact_export(
        &self,
        artifact_id: &str,
    ) -> MedusaResult<crate::FrontendArtifactExport> {
        match self.request(Request::FrontendArtifactExport {
            artifact_id: artifact_id.to_owned(),
        })? {
            Response::FrontendArtifactExport { artifact } => Ok(artifact),
            Response::Error { code, message } => Err(frontend_request_error(code, message)),
            response => Err(MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                format!("daemon returned an unexpected artifact export response: {response:?}"),
            )),
        }
    }

    /// Updates one process-local daemon credential without persisting it in protocol evidence.
    pub fn frontend_credential(&self, update: FrontendCredentialUpdate) -> MedusaResult<()> {
        match self.request(Request::FrontendCredential { update })? {
            Response::Ack => Ok(()),
            Response::Error { code, message } => Err(frontend_request_error(code, message)),
            response => Err(MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                format!("daemon returned an unexpected credential response: {response:?}"),
            )),
        }
    }
}

fn frontend_request_error(code: String, message: String) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Environment,
        format!("daemon frontend request failed ({code}): {message}"),
    )
}

/// Starts a daemon loop with production limits and blocks until shutdown.
pub fn serve(paths: DaemonPaths) -> MedusaResult<()> {
    let config = load_repository_config(&paths.repo)?;
    serve_with_config(paths, config)
}

fn load_repository_config(repo: &Path) -> MedusaResult<Config> {
    let project = repo.join(".medusa/config.toml");
    let project = project.exists().then_some(project);
    Config::load_layers(None, project.as_deref(), &BTreeMap::new(), &BTreeMap::new())
}

#[cfg(test)]
mod repository_config_tests {
    use super::*;

    #[test]
    fn daemon_loads_repository_agent_limits() {
        let directory = tempfile::tempdir().expect("tempdir");
        let medusa = directory.path().join(".medusa");
        fs::create_dir_all(&medusa).expect("create .medusa");
        fs::write(
            medusa.join("config.toml"),
            "[agent]\nmax_turns = 24\nparallel_workers = 1\n",
        )
        .expect("write project config");

        let config = load_repository_config(directory.path()).expect("load repository config");
        assert_eq!(config.agent.max_turns, 24);
        assert_eq!(config.agent.parallel_workers, 1);
    }
}

/// Starts a daemon loop with production limits and an explicit resolved configuration.
pub fn serve_with_config(paths: DaemonPaths, config: Config) -> MedusaResult<()> {
    serve_with_limits_and_config(paths, DaemonLimits::default(), config)
}

/// Starts a daemon loop with explicit worker and queue limits.
pub fn serve_with_limits(paths: DaemonPaths, limits: DaemonLimits) -> MedusaResult<()> {
    let config = load_repository_config(&paths.repo)?;
    serve_with_limits_and_config(paths, limits, config)
}

fn serve_with_limits_and_config(
    paths: DaemonPaths,
    limits: DaemonLimits,
    config: Config,
) -> MedusaResult<()> {
    fs::create_dir_all(&paths.directory)?;
    let _ownership = Ownership::acquire(&paths)?;
    let (jobs, recovered) = load_and_recover(&paths)?;
    if recovered {
        persist_jobs(&paths, &jobs)?;
    }
    let jobs = Arc::new(Mutex::new(jobs));
    let processes = Arc::new(ProcessRegistry::default());
    let frontend = Arc::new(Mutex::new(FrontendControlPlane::new(
        paths.repo.clone(),
        config,
    )));
    let listener = LocalListener::bind(&paths.socket).map_err(transport_error)?;
    let scheduler = match start_scheduler(&paths, &jobs, &processes, limits) {
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
        processes,
        frontend,
        Arc::new(AtomicU8::new(SHUTDOWN_NONE)),
        scheduler,
    )
}

/// Starts the server in a dedicated thread with production limits.
pub fn spawn(
    paths: DaemonPaths,
) -> MedusaResult<(ServerHandle, thread::JoinHandle<MedusaResult<()>>)> {
    spawn_with_config(paths, Config::default())
}

/// Starts the server in a dedicated thread with an explicit resolved configuration.
pub fn spawn_with_config(
    paths: DaemonPaths,
    config: Config,
) -> MedusaResult<(ServerHandle, thread::JoinHandle<MedusaResult<()>>)> {
    spawn_with_limits_and_config(paths, DaemonLimits::default(), config)
}

/// Starts the server in a dedicated thread with explicit worker and queue limits.
pub fn spawn_with_limits(
    paths: DaemonPaths,
    limits: DaemonLimits,
) -> MedusaResult<(ServerHandle, thread::JoinHandle<MedusaResult<()>>)> {
    spawn_with_limits_and_config(paths, limits, Config::default())
}

fn spawn_with_limits_and_config(
    paths: DaemonPaths,
    limits: DaemonLimits,
    config: Config,
) -> MedusaResult<(ServerHandle, thread::JoinHandle<MedusaResult<()>>)> {
    fs::create_dir_all(&paths.directory)?;
    limits.validate()?;
    let shutdown = Arc::new(AtomicU8::new(SHUTDOWN_NONE));
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
            let processes = Arc::new(ProcessRegistry::default());
            let frontend = Arc::new(Mutex::new(FrontendControlPlane::new(
                paths.repo.clone(),
                config,
            )));
            let listener = LocalListener::bind(&paths.socket).map_err(transport_error)?;
            let scheduler = match start_scheduler(&paths, &jobs, &processes, limits) {
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
                processes,
                frontend,
                server_shutdown,
                scheduler,
            )
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
    processes: &Arc<ProcessRegistry>,
    limits: DaemonLimits,
) -> MedusaResult<JobScheduler> {
    let worker_paths = paths.clone();
    let worker_jobs = Arc::clone(jobs);
    let worker_processes = Arc::clone(processes);
    let runner: JobRunner = Arc::new(move |job_id| {
        run_job(&worker_paths, &worker_jobs, &worker_processes, &job_id);
    });
    JobScheduler::start(limits, runner)
}

type SharedJobScheduler = Arc<Mutex<JobScheduler>>;

struct ConnectionWorkerPool {
    sender: Option<SyncSender<LocalStream>>,
    workers: Vec<thread::JoinHandle<()>>,
}

impl ConnectionWorkerPool {
    fn start(
        paths: DaemonPaths,
        jobs: Arc<Mutex<BTreeMap<String, JobRecord>>>,
        processes: Arc<ProcessRegistry>,
        frontend: Arc<Mutex<FrontendControlPlane>>,
        shutdown: Arc<AtomicU8>,
        scheduler: SharedJobScheduler,
    ) -> MedusaResult<Self> {
        let (sender, receiver) = mpsc::sync_channel(CONNECTION_QUEUE_CAPACITY);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut pool = Self {
            sender: Some(sender),
            workers: Vec::with_capacity(CONNECTION_WORKERS),
        };

        for index in 0..CONNECTION_WORKERS {
            let worker_receiver = Arc::clone(&receiver);
            let worker_paths = paths.clone();
            let worker_jobs = Arc::clone(&jobs);
            let worker_processes = Arc::clone(&processes);
            let worker_frontend = Arc::clone(&frontend);
            let worker_shutdown = Arc::clone(&shutdown);
            let worker_scheduler = Arc::clone(&scheduler);
            match thread::Builder::new()
                .name(format!("medusa-connection-worker-{index}"))
                .spawn(move || {
                    connection_worker_loop(
                        worker_receiver,
                        worker_paths,
                        worker_jobs,
                        worker_processes,
                        worker_frontend,
                        worker_shutdown,
                        worker_scheduler,
                    );
                })
            {
                Ok(worker) => pool.workers.push(worker),
                Err(error) => {
                    let _ = pool.shutdown();
                    return Err(MedusaError::new(
                        ErrorCode::DependencyUnavailable,
                        ErrorCategory::Environment,
                        format!("failed to spawn daemon connection worker {index}: {error}"),
                    ));
                }
            }
        }
        Ok(pool)
    }

    fn enqueue(&self, stream: LocalStream) -> Result<(), LocalStream> {
        let Some(sender) = self.sender.as_ref() else {
            return Err(stream);
        };
        match sender.try_send(stream) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(stream) | TrySendError::Disconnected(stream)) => Err(stream),
        }
    }

    fn shutdown(&mut self) -> MedusaResult<()> {
        self.sender.take();
        let mut first_error = None;
        for worker in self.workers.drain(..) {
            if worker.join().is_err() && first_error.is_none() {
                first_error = Some(MedusaError::new(
                    ErrorCode::InternalInvariant,
                    ErrorCategory::Internal,
                    "daemon connection worker terminated unexpectedly",
                ));
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

impl Drop for ConnectionWorkerPool {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn connection_worker_loop(
    receiver: Arc<Mutex<Receiver<LocalStream>>>,
    paths: DaemonPaths,
    jobs: Arc<Mutex<BTreeMap<String, JobRecord>>>,
    processes: Arc<ProcessRegistry>,
    frontend: Arc<Mutex<FrontendControlPlane>>,
    shutdown: Arc<AtomicU8>,
    scheduler: SharedJobScheduler,
) {
    loop {
        if shutdown.load(Ordering::SeqCst) != SHUTDOWN_NONE {
            return;
        }
        let stream = {
            let receiver = receiver
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match receiver.recv() {
                Ok(stream) => stream,
                Err(_) => return,
            }
        };
        if shutdown.load(Ordering::SeqCst) != SHUTDOWN_NONE {
            return;
        }
        let _ = handle_connection(
            stream,
            &paths,
            &jobs,
            &processes,
            &frontend,
            &shutdown,
            &scheduler,
        );
    }
}

fn run_loop(
    listener: LocalListener,
    paths: DaemonPaths,
    jobs: Arc<Mutex<BTreeMap<String, JobRecord>>>,
    processes: Arc<ProcessRegistry>,
    frontend: Arc<Mutex<FrontendControlPlane>>,
    shutdown: Arc<AtomicU8>,
    scheduler: JobScheduler,
) -> MedusaResult<()> {
    let scheduler = Arc::new(Mutex::new(scheduler));
    let mut connections = match ConnectionWorkerPool::start(
        paths.clone(),
        Arc::clone(&jobs),
        Arc::clone(&processes),
        Arc::clone(&frontend),
        Arc::clone(&shutdown),
        Arc::clone(&scheduler),
    ) {
        Ok(connections) => connections,
        Err(error) => {
            listener.cleanup();
            return Err(error);
        }
    };

    let result = (|| {
        while shutdown.load(Ordering::SeqCst) == SHUTDOWN_NONE {
            match listener.accept() {
                Ok(stream) => {
                    if let Err(stream) = connections.enqueue(stream) {
                        reject_busy_connection(stream);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(transport_error(error)),
            }
        }
        Ok(())
    })();
    let connection_result = connections.shutdown();
    let (cancellation_result, scheduler_result) = match lock_scheduler(&scheduler) {
        Ok(mut scheduler) => {
            let cancellation_result = if shutdown.load(Ordering::SeqCst) == SHUTDOWN_IMMEDIATE {
                cancel_all_jobs(&paths, &jobs, &processes, &scheduler)
            } else {
                Ok(())
            };
            let scheduler_result = scheduler.shutdown();
            (cancellation_result, scheduler_result)
        }
        Err(error) => (Err(error), Ok(())),
    };
    listener.cleanup();
    match (
        result,
        connection_result,
        cancellation_result,
        scheduler_result,
    ) {
        (Err(error), _, _, _)
        | (Ok(()), Err(error), _, _)
        | (Ok(()), Ok(()), Err(error), _)
        | (Ok(()), Ok(()), Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(()), Ok(()), Ok(())) => Ok(()),
    }
}

fn reject_busy_connection(mut stream: LocalStream) {
    let _ = stream.set_write_timeout(Some(REQUEST_IO_TIMEOUT));
    let _ = write_response(
        &mut stream,
        Response::Error {
            code: "daemon_busy".to_owned(),
            message: "daemon connection queue is at capacity; retry later".to_owned(),
        },
    );
}

fn handle_connection(
    mut stream: LocalStream,
    paths: &DaemonPaths,
    jobs: &Arc<Mutex<BTreeMap<String, JobRecord>>>,
    processes: &Arc<ProcessRegistry>,
    frontend: &Arc<Mutex<FrontendControlPlane>>,
    shutdown: &Arc<AtomicU8>,
    scheduler: &SharedJobScheduler,
) -> MedusaResult<()> {
    stream
        .set_read_timeout(Some(REQUEST_IO_TIMEOUT))
        .map_err(transport_error)?;
    stream
        .set_write_timeout(Some(REQUEST_IO_TIMEOUT))
        .map_err(transport_error)?;
    let reader_stream = stream.try_clone().map_err(transport_error)?;
    let mut reader = BufReader::new(reader_stream).take((MAX_ARTIFACT_REQUEST_BYTES + 1) as u64);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if line.trim().is_empty() {
        return Ok(());
    }
    if line.len() > MAX_ARTIFACT_REQUEST_BYTES {
        return write_response(
            &mut stream,
            Response::Error {
                code: "request_too_large".into(),
                message: format!(
                    "daemon artifact request exceeds {MAX_ARTIFACT_REQUEST_BYTES} bytes"
                ),
            },
        );
    }
    let envelope: RequestEnvelope = serde_json::from_str(&line)?;
    if line.len() > MAX_REQUEST_BYTES
        && !matches!(&envelope.request, Request::FrontendArtifact { .. })
    {
        return write_response(
            &mut stream,
            Response::Error {
                code: "request_too_large".into(),
                message: format!("daemon request exceeds {MAX_REQUEST_BYTES} bytes"),
            },
        );
    }
    let response = if envelope.version != DAEMON_PROTOCOL_VERSION {
        Response::Error {
            code: "incompatible_protocol".into(),
            message: format!("unsupported protocol {}", envelope.version),
        }
    } else {
        dispatch(
            envelope.request,
            paths,
            jobs,
            processes,
            frontend,
            shutdown,
            scheduler,
        )?
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
    processes: &Arc<ProcessRegistry>,
    frontend: &Arc<Mutex<FrontendControlPlane>>,
    shutdown: &Arc<AtomicU8>,
    scheduler: &SharedJobScheduler,
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
            processes.register(&job.id)?;
            {
                let mut locked = lock_jobs(jobs)?;
                locked.insert(job.id.clone(), job.clone());
                if let Err(error) = persist_jobs(paths, &locked) {
                    locked.remove(&job.id);
                    let _ = processes.remove(&job.id);
                    return Err(error);
                }
            }
            let scheduler = lock_scheduler(scheduler)?;
            match scheduler.enqueue(job.id.clone()) {
                Ok(()) => Ok(Response::Submitted { job }),
                Err(SubmitError::Busy) => {
                    discard_rejected_job(paths, jobs, processes, &job.id)?;
                    Ok(Response::Error {
                        code: "daemon_busy".into(),
                        message: "daemon job queue is at capacity; retry later".into(),
                    })
                }
                Err(SubmitError::Stopped) => {
                    discard_rejected_job(paths, jobs, processes, &job.id)?;
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
        Request::Cancel { job_id } => {
            let scheduler = lock_scheduler(scheduler)?;
            cancel_job(paths, jobs, processes, &scheduler, &job_id)
        }
        Request::List => {
            let locked = lock_jobs(jobs)?;
            Ok(Response::Jobs {
                jobs: locked.values().cloned().collect(),
            })
        }
        Request::Frontend { envelope } => {
            let mut control = lock_frontend(frontend)?;
            Ok(match control.dispatch(envelope) {
                Ok(acknowledgement) => Response::Frontend { acknowledgement },
                Err(error) => Response::Error {
                    code: "frontend_control".to_owned(),
                    message: error.to_string(),
                },
            })
        }
        Request::FrontendArtifact { upload } => {
            let bytes = match STANDARD.decode(upload.bytes_base64.as_bytes()) {
                Ok(bytes) => bytes,
                Err(error) => {
                    return Ok(Response::Error {
                        code: "invalid_artifact_encoding".to_owned(),
                        message: format!("frontend artifact is not valid base64: {error}"),
                    });
                }
            };
            let control = lock_frontend(frontend)?;
            Ok(
                match control.ingest_artifact(
                    upload.display_name,
                    upload.mime_type,
                    upload.kind,
                    bytes,
                ) {
                    Ok(artifact_id) => Response::FrontendArtifact { artifact_id },
                    Err(error) => Response::Error {
                        code: "frontend_artifact".to_owned(),
                        message: error.to_string(),
                    },
                },
            )
        }
        Request::FrontendArtifactExport { artifact_id } => {
            let control = lock_frontend(frontend)?;
            Ok(match control.export_attachment(&artifact_id) {
                Ok(artifact) => Response::FrontendArtifactExport { artifact },
                Err(error) => Response::Error {
                    code: "frontend_artifact_export".to_owned(),
                    message: error.to_string(),
                },
            })
        }
        Request::FrontendCredential { update } => {
            let mut control = lock_frontend(frontend)?;
            Ok(
                match control.update_credential(update.provider, update.credential) {
                    Ok(()) => Response::Ack,
                    Err(error) => Response::Error {
                        code: "frontend_credential".to_owned(),
                        message: error.to_string(),
                    },
                },
            )
        }
        Request::Shutdown => {
            request_shutdown(shutdown, SHUTDOWN_GRACEFUL);
            Ok(Response::Ack)
        }
        Request::ShutdownNow => {
            request_shutdown(shutdown, SHUTDOWN_IMMEDIATE);
            Ok(Response::Ack)
        }
    }
}

fn request_shutdown(shutdown: &AtomicU8, mode: u8) {
    shutdown.fetch_max(mode, Ordering::SeqCst);
}

fn discard_rejected_job(
    paths: &DaemonPaths,
    jobs: &Arc<Mutex<BTreeMap<String, JobRecord>>>,
    processes: &ProcessRegistry,
    job_id: &str,
) -> MedusaResult<()> {
    let mut locked = lock_jobs(jobs)?;
    locked.remove(job_id);
    persist_jobs(paths, &locked)?;
    processes.remove(job_id)
}

fn run_job(
    paths: &DaemonPaths,
    jobs: &Arc<Mutex<BTreeMap<String, JobRecord>>>,
    processes: &ProcessRegistry,
    job_id: &str,
) {
    if let Err(error) = run_job_inner(paths, jobs, processes, job_id) {
        record_worker_error(paths, jobs, processes, job_id, error);
    }
    if let Err(error) = processes.remove(job_id) {
        record_worker_error(paths, jobs, processes, job_id, error);
    }
}

fn run_job_inner(
    paths: &DaemonPaths,
    jobs: &Arc<Mutex<BTreeMap<String, JobRecord>>>,
    processes: &ProcessRegistry,
    job_id: &str,
) -> MedusaResult<()> {
    if processes.is_cancelled(job_id)? {
        mark_job_interrupted(paths, jobs, job_id, "cancelled before process start")?;
        return Ok(());
    }
    let command = {
        let mut locked = lock_jobs(jobs)?;
        let Some(job) = locked.get_mut(job_id) else {
            return Err(MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                format!("daemon queued job disappeared before execution: {job_id}"),
            ));
        };
        if job.state == JobState::Interrupted {
            return Ok(());
        }
        job.state = JobState::Running;
        job.started_at = Some(OffsetDateTime::now_utc());
        let command = (job.program.clone(), job.args.clone());
        persist_jobs(paths, &locked)?;
        command
    };

    let output = processes.run(
        job_id,
        &command.0,
        &command.1,
        &paths.repo,
        &paths.directory,
    )?;
    let Some(output) = output else {
        mark_job_interrupted(paths, jobs, job_id, "cancelled before process start")?;
        return Ok(());
    };
    let mut locked = lock_jobs(jobs)?;
    let Some(job) = locked.get_mut(job_id) else {
        return Err(MedusaError::new(
            ErrorCode::InternalInvariant,
            ErrorCategory::Internal,
            format!("daemon running job disappeared before completion: {job_id}"),
        ));
    };
    job.finished_at = Some(OffsetDateTime::now_utc());
    job.exit_code = output.status.code();
    job.stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let process_stderr = String::from_utf8_lossy(&output.stderr);
    if output.cancelled || job.state == JobState::Interrupted {
        if !process_stderr.trim().is_empty() {
            append_detail(&mut job.stderr, process_stderr.trim());
        }
        append_detail(
            &mut job.stderr,
            "process tree terminated after cancellation",
        );
        job.state = JobState::Interrupted;
    } else {
        job.stderr = process_stderr.into_owned();
        job.state = if output.status.success() {
            JobState::Succeeded
        } else {
            JobState::Failed
        };
    }
    persist_jobs(paths, &locked)
}

fn record_worker_error(
    paths: &DaemonPaths,
    jobs: &Arc<Mutex<BTreeMap<String, JobRecord>>>,
    processes: &ProcessRegistry,
    job_id: &str,
    error: MedusaError,
) {
    let message = format!("daemon worker failed: {error}");
    match processes.is_cancelled(job_id) {
        Ok(true) => {
            let _ = mark_job_interrupted(paths, jobs, job_id, &message);
        }
        Ok(false) | Err(_) => mark_job_failed(paths, jobs, job_id, message),
    }
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
    if job.state == JobState::Interrupted {
        append_detail(&mut job.stderr, &message);
    } else {
        job.state = JobState::Failed;
        job.finished_at = Some(OffsetDateTime::now_utc());
        job.stderr = message;
    }
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

pub(crate) fn persist_jobs(
    paths: &DaemonPaths,
    jobs: &BTreeMap<String, JobRecord>,
) -> MedusaResult<()> {
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

fn lock_frontend(
    frontend: &Arc<Mutex<FrontendControlPlane>>,
) -> MedusaResult<std::sync::MutexGuard<'_, FrontendControlPlane>> {
    #[cfg(test)]
    {
        let target = FRONTEND_LOCK_PROBE_TARGET.load(Ordering::SeqCst);
        if target != 0 && Arc::as_ptr(frontend) as usize == target {
            FRONTEND_LOCK_PROBE_HITS.fetch_add(1, Ordering::SeqCst);
        }
    }
    frontend.lock().map_err(|_| {
        MedusaError::new(
            ErrorCode::InternalInvariant,
            ErrorCategory::Internal,
            "daemon frontend control lock was poisoned",
        )
    })
}

fn lock_scheduler(
    scheduler: &SharedJobScheduler,
) -> MedusaResult<std::sync::MutexGuard<'_, JobScheduler>> {
    scheduler.lock().map_err(|_| {
        MedusaError::new(
            ErrorCode::InternalInvariant,
            ErrorCategory::Internal,
            "daemon job scheduler lock was poisoned",
        )
    })
}

pub(crate) fn lock_jobs(
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
    medusa_process_containment::process_is_alive(pid)
}

#[cfg(test)]
mod connection_concurrency_tests {
    use std::time::Instant;

    use medusa_protocol::frontend::{
        FRONTEND_PROTOCOL_VERSION, FrontendCommand, FrontendCommandEnvelope, FrontendKind,
    };

    use super::*;

    struct ProbeReset;

    impl Drop for ProbeReset {
        fn drop(&mut self) {
            FRONTEND_LOCK_PROBE_TARGET.store(0, Ordering::SeqCst);
            FRONTEND_LOCK_PROBE_HITS.store(0, Ordering::SeqCst);
        }
    }

    #[test]
    fn health_ping_remains_responsive_while_frontend_dispatch_is_blocked() {
        let _probe_reset = ProbeReset;
        let directory = tempfile::tempdir().expect("tempdir");
        let paths = DaemonPaths::for_repo(directory.path());
        fs::create_dir_all(&paths.directory).expect("daemon directory");
        let jobs = Arc::new(Mutex::new(BTreeMap::new()));
        let processes = Arc::new(ProcessRegistry::default());
        let frontend = Arc::new(Mutex::new(FrontendControlPlane::new(
            paths.repo.clone(),
            Config::default(),
        )));
        let listener = LocalListener::bind(&paths.socket).expect("bind daemon listener");
        let scheduler = start_scheduler(&paths, &jobs, &processes, DaemonLimits::default())
            .expect("start scheduler");
        let shutdown = Arc::new(AtomicU8::new(SHUTDOWN_NONE));
        let frontend_guard = frontend.lock().expect("hold frontend lock");
        FRONTEND_LOCK_PROBE_TARGET.store(Arc::as_ptr(&frontend) as usize, Ordering::SeqCst);
        FRONTEND_LOCK_PROBE_HITS.store(0, Ordering::SeqCst);

        let server = {
            let paths = paths.clone();
            let jobs = Arc::clone(&jobs);
            let processes = Arc::clone(&processes);
            let frontend = Arc::clone(&frontend);
            let shutdown = Arc::clone(&shutdown);
            thread::spawn(move || {
                run_loop(
                    listener, paths, jobs, processes, frontend, shutdown, scheduler,
                )
            })
        };

        let envelope = FrontendCommandEnvelope {
            protocol_version: FRONTEND_PROTOCOL_VERSION,
            command_id: "blocked-frontend".to_owned(),
            idempotency_key: "blocked-frontend".to_owned(),
            frontend: FrontendKind::Tui,
            client_id: "blocked-frontend-client".to_owned(),
            session_id: None,
            turn_id: None,
            timestamp: OffsetDateTime::now_utc(),
            command: FrontendCommand::ListSessions,
        };
        let blocked_client = DaemonClient::new(&paths.socket);
        let blocked_request = thread::spawn(move || blocked_client.frontend(envelope));

        let deadline = Instant::now() + Duration::from_secs(2);
        while FRONTEND_LOCK_PROBE_HITS.load(Ordering::SeqCst) == 0 {
            assert!(
                Instant::now() < deadline,
                "frontend request did not reach the blocked control-plane lock"
            );
            thread::sleep(Duration::from_millis(10));
        }

        let started = Instant::now();
        let response = DaemonClient::new(&paths.socket)
            .request(Request::Ping)
            .expect("health ping while frontend request is blocked");
        assert!(matches!(response, Response::Pong));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "health ping waited behind a blocked frontend request"
        );

        drop(frontend_guard);
        blocked_request
            .join()
            .expect("join blocked request")
            .expect("blocked frontend request completes");
        assert!(matches!(
            DaemonClient::new(&paths.socket)
                .request(Request::Shutdown)
                .expect("shutdown daemon"),
            Response::Ack
        ));
        server.join().expect("join server").expect("server result");
    }
}

#[cfg(test)]
mod tests;
