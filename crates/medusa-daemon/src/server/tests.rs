use std::time::Instant;

use medusa_protocol::frontend::{
    FRONTEND_PROTOCOL_VERSION, FrontendCommand, FrontendCommandEnvelope, FrontendKind,
};

use crate::FrontendControlResult;

use super::*;

fn wait_for_endpoint(path: &Path) {
    for _ in 0..200 {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("daemon endpoint did not appear: {}", path.display());
}

fn submit_job(client: &DaemonClient, command: (String, Vec<String>)) -> JobRecord {
    let Response::Submitted { job } = client
        .request(Request::Submit {
            program: command.0,
            args: command.1,
        })
        .expect("submit job")
    else {
        panic!("unexpected submit response");
    };
    job
}

fn wait_for_state(client: &DaemonClient, job_id: &str, expected: JobState) -> JobRecord {
    for _ in 0..250 {
        if let Ok(Response::Status { job: Some(job) }) = client.request(Request::Status {
            job_id: job_id.to_owned(),
        }) {
            if job.state == expected {
                return job;
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("daemon job {job_id} did not reach {expected:?}");
}

#[cfg(unix)]
const PYTHON: &str = "python3";
#[cfg(windows)]
const PYTHON: &str = "python.exe";

fn python_command(
    script: &str,
    extra_args: impl IntoIterator<Item = String>,
) -> (String, Vec<String>) {
    let mut args = vec!["-c".to_owned(), script.to_owned()];
    args.extend(extra_args);
    (PYTHON.to_owned(), args)
}

fn delayed_command() -> (String, Vec<String>) {
    python_command(
        "import pathlib,time; time.sleep(0.3); pathlib.Path('finished.txt').write_text('done'); print('verified-daemon-reconnect', end='')",
        [],
    )
}

fn blocking_command() -> (String, Vec<String>) {
    python_command("import time; time.sleep(10)", [])
}

fn marker_command(marker: &Path) -> (String, Vec<String>) {
    python_command(
        "import pathlib,sys; pathlib.Path(sys.argv[1]).write_text('should-not-run')",
        [marker.to_string_lossy().into_owned()],
    )
}

fn descendant_command(marker: &Path) -> (String, Vec<String>) {
    python_command(
        r#"import pathlib,subprocess,sys,time; subprocess.Popen([sys.executable,"-c","import pathlib,sys,time; time.sleep(1); pathlib.Path(sys.argv[1]).write_text('orphan')",sys.argv[1]]); time.sleep(1)"#,
        [marker.to_string_lossy().into_owned()],
    )
}

fn spawn_unrelated_process() -> std::process::Child {
    let (program, args) = blocking_command();
    std::process::Command::new(program)
        .args(args)
        .spawn()
        .expect("spawn unrelated process")
}

#[test]
fn canonical_frontend_command_round_trips_over_daemon_wire() {
    let directory = tempfile::tempdir().expect("tempdir");
    let paths = DaemonPaths::for_repo(directory.path());
    let (handle, server) = spawn(paths.clone()).expect("spawn daemon");
    wait_for_endpoint(&paths.socket);
    let client = DaemonClient::new(&paths.socket);
    let envelope = FrontendCommandEnvelope {
        protocol_version: FRONTEND_PROTOCOL_VERSION,
        command_id: "desktop-list-1".to_owned(),
        idempotency_key: "desktop:list:1".to_owned(),
        frontend: FrontendKind::Desktop,
        client_id: "desktop-client".to_owned(),
        session_id: None,
        turn_id: None,
        timestamp: OffsetDateTime::now_utc(),
        command: FrontendCommand::ListSessions,
    };

    let first = client.frontend(envelope.clone()).expect("frontend request");
    let FrontendControlResult::Sessions { sessions } = &first.result else {
        panic!("expected sessions response")
    };
    assert!(sessions.is_empty());
    let duplicate = client.frontend(envelope).expect("idempotent replay");
    assert_eq!(first, duplicate);

    handle.shutdown();
    server.join().expect("join daemon").expect("daemon result");
}

#[test]
fn client_reconnects_while_job_continues() {
    let directory = tempfile::tempdir().expect("tempdir");
    let paths = DaemonPaths::for_repo(directory.path());
    let (handle, server) = spawn(paths.clone()).expect("spawn daemon");
    wait_for_endpoint(&paths.socket);

    let first_client = DaemonClient::new(&paths.socket);
    let job = submit_job(&first_client, delayed_command());
    drop(first_client);

    let second_client = DaemonClient::new(&paths.socket);
    let completed = (0..750).find_map(|_| {
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
    assert_eq!(
        completed.state,
        JobState::Succeeded,
        "daemon job stderr: {}",
        completed.stderr
    );
    assert!(completed.stdout.contains("verified-daemon-reconnect"));
    assert_eq!(
        fs::read_to_string(directory.path().join("finished.txt")).expect("finished file"),
        "done"
    );

    handle.shutdown();
    server.join().expect("join daemon").expect("daemon result");
    assert!(!paths.socket.exists());
}

#[test]
fn queued_job_cancellation_prevents_execution() {
    let directory = tempfile::tempdir().expect("tempdir");
    let paths = DaemonPaths::for_repo(directory.path());
    let limits = DaemonLimits {
        max_concurrent_jobs: 1,
        max_queued_jobs: 1,
    };
    let (handle, server) = spawn_with_limits(paths.clone(), limits).expect("spawn daemon");
    wait_for_endpoint(&paths.socket);
    let client = DaemonClient::new(&paths.socket);

    let running = submit_job(&client, blocking_command());
    wait_for_state(&client, &running.id, JobState::Running);
    let marker = directory.path().join("cancelled-queue.txt");
    let queued = submit_job(&client, marker_command(&marker));
    let Response::Cancelled {
        job: Some(cancelled),
    } = client
        .request(Request::Cancel {
            job_id: queued.id.clone(),
        })
        .expect("cancel queued job")
    else {
        panic!("unexpected cancellation response");
    };
    assert_eq!(cancelled.state, JobState::Interrupted);

    handle.shutdown_now();
    server.join().expect("join daemon").expect("daemon result");
    assert!(!marker.exists(), "cancelled queued job must never execute");
}

#[test]
fn running_cancellation_terminates_descendants_but_not_unrelated_processes() {
    let directory = tempfile::tempdir().expect("tempdir");
    let paths = DaemonPaths::for_repo(directory.path());
    let (handle, server) = spawn(paths.clone()).expect("spawn daemon");
    wait_for_endpoint(&paths.socket);
    let client = DaemonClient::new(&paths.socket);
    let marker = directory.path().join("orphan.txt");
    let job = submit_job(&client, descendant_command(&marker));
    wait_for_state(&client, &job.id, JobState::Running);
    let mut unrelated = spawn_unrelated_process();

    let started = Instant::now();
    let Response::Cancelled {
        job: Some(cancelled),
    } = client
        .request(Request::Cancel {
            job_id: job.id.clone(),
        })
        .expect("cancel running job")
    else {
        panic!("unexpected cancellation response");
    };
    assert_eq!(cancelled.state, JobState::Interrupted);
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "running cancellation exceeded its bounded interval"
    );

    handle.shutdown();
    server.join().expect("join daemon").expect("daemon result");
    thread::sleep(Duration::from_millis(1200));
    assert!(!marker.exists(), "cancelled descendant wrote its marker");
    assert!(
        unrelated
            .try_wait()
            .expect("inspect unrelated process")
            .is_none(),
        "cancelling a daemon job terminated an unrelated process"
    );
    unrelated.kill().expect("kill unrelated process");
    unrelated.wait().expect("wait unrelated process");
}

#[test]
fn immediate_shutdown_cancels_running_jobs_within_a_bound() {
    let directory = tempfile::tempdir().expect("tempdir");
    let paths = DaemonPaths::for_repo(directory.path());
    let (handle, server) = spawn(paths.clone()).expect("spawn daemon");
    wait_for_endpoint(&paths.socket);
    let client = DaemonClient::new(&paths.socket);
    let job = submit_job(&client, blocking_command());
    wait_for_state(&client, &job.id, JobState::Running);

    let started = Instant::now();
    handle.shutdown_now();
    server.join().expect("join daemon").expect("daemon result");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "immediate shutdown waited for the original child duration"
    );
    let (jobs, recovered) = load_and_recover(&paths).expect("load persisted jobs");
    assert!(!recovered, "immediate shutdown must persist terminal state");
    assert_eq!(jobs[&job.id].state, JobState::Interrupted);
}

#[test]
fn restart_marks_orphaned_jobs_interrupted() {
    let directory = tempfile::tempdir().expect("tempdir");
    let paths = DaemonPaths::for_repo(directory.path());
    fs::create_dir_all(&paths.directory).expect("daemon directory");
    let job = JobRecord {
        id: "job-test".into(),
        program: "test".into(),
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

#[test]
fn backup_state_is_restored_after_interrupted_replacement() {
    let directory = tempfile::tempdir().expect("tempdir");
    let paths = DaemonPaths::for_repo(directory.path());
    fs::create_dir_all(&paths.directory).expect("daemon directory");
    let job = JobRecord {
        id: "job-backup".into(),
        program: "test".into(),
        args: vec![],
        state: JobState::Succeeded,
        created_at: OffsetDateTime::now_utc(),
        started_at: None,
        finished_at: Some(OffsetDateTime::now_utc()),
        exit_code: Some(0),
        stdout: "saved".into(),
        stderr: String::new(),
    };
    fs::write(
        backup_path(&paths.state),
        serde_json::to_vec_pretty(&BTreeMap::from([(job.id.clone(), job)])).expect("serialize"),
    )
    .expect("backup state");

    let (jobs, recovered) = load_and_recover(&paths).expect("recover backup");
    assert!(!recovered);
    assert_eq!(jobs["job-backup"].stdout, "saved");
    assert!(paths.state.exists());
}

#[test]
fn active_owner_is_not_replaced() {
    let directory = tempfile::tempdir().expect("tempdir");
    let paths = DaemonPaths::for_repo(directory.path());
    fs::create_dir_all(&paths.directory).expect("daemon directory");
    fs::write(&paths.owner, std::process::id().to_string()).expect("active owner");

    let error = match Ownership::acquire(&paths) {
        Ok(_) => panic!("active owner must be retained"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::PolicyDenied);
    assert!(paths.owner.exists());
}

#[test]
fn stale_owner_is_recovered_before_restart() {
    let directory = tempfile::tempdir().expect("tempdir");
    let paths = DaemonPaths::for_repo(directory.path());
    fs::create_dir_all(&paths.directory).expect("daemon directory");
    fs::write(&paths.owner, b"999999").expect("stale owner");

    let (handle, server) = spawn(paths.clone()).expect("spawn daemon");
    wait_for_endpoint(&paths.socket);
    assert!(matches!(
        DaemonClient::new(&paths.socket).request(Request::Ping),
        Ok(Response::Pong)
    ));

    handle.shutdown();
    server.join().expect("join daemon").expect("daemon result");
    assert!(!paths.owner.exists());
}

#[test]
fn daemon_paths_remain_repository_scoped() {
    let paths = DaemonPaths::for_repo(Path::new("workspace"));
    assert_eq!(
        paths.socket,
        PathBuf::from("workspace/.medusa/daemon/medusa.sock")
    );
    assert_eq!(
        paths.state,
        PathBuf::from("workspace/.medusa/daemon/jobs.json")
    );
    assert_eq!(
        paths.owner,
        PathBuf::from("workspace/.medusa/daemon/owner.pid")
    );
}
