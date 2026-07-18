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

#[cfg(unix)]
fn delayed_command() -> (String, Vec<String>) {
    (
        "sh".to_owned(),
        vec![
            "-c".to_owned(),
            "sleep 0.3; printf done > finished.txt; printf verified-daemon-reconnect".to_owned(),
        ],
    )
}

#[cfg(windows)]
fn delayed_command() -> (String, Vec<String>) {
    (
        "cmd".to_owned(),
        vec![
            "/C".to_owned(),
            "ping -n 2 127.0.0.1 >NUL & echo|set /p=done>finished.txt & echo|set /p=verified-daemon-reconnect"
                .to_owned(),
        ],
    )
}

#[test]
fn client_reconnects_while_job_continues() {
    let directory = tempfile::tempdir().expect("tempdir");
    let paths = DaemonPaths::for_repo(directory.path());
    let (handle, server) = spawn(paths.clone()).expect("spawn daemon");
    wait_for_endpoint(&paths.socket);

    let (program, args) = delayed_command();
    let first_client = DaemonClient::new(&paths.socket);
    let Response::Submitted { job } = first_client
        .request(Request::Submit { program, args })
        .expect("submit")
    else {
        panic!("unexpected submit response");
    };
    drop(first_client);

    let second_client = DaemonClient::new(&paths.socket);
    let completed = (0..250).find_map(|_| {
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
    assert!(!paths.socket.exists());
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
