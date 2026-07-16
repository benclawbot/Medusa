#![cfg(unix)]

use std::{fs, thread, time::Duration};

use medusa_daemon::{DaemonClient, DaemonPaths, JobRecord, JobState, Request, Response, spawn};
use time::OffsetDateTime;

fn wait_for_socket(path: &std::path::Path) {
    for _ in 0..100 {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("socket did not appear: {}", path.display());
}

#[test]
fn daemon_records_missing_program_failure_and_persists_it() {
    let directory = tempfile::tempdir().expect("tempdir");
    let paths = DaemonPaths::for_repo(directory.path());
    let (handle, server) = spawn(paths.clone()).expect("spawn daemon");
    wait_for_socket(&paths.socket);
    let client = DaemonClient::new(&paths.socket);

    let Response::Submitted { job } = client
        .request(Request::Submit {
            program: "medusa-daemon-program-that-does-not-exist".into(),
            args: Vec::new(),
        })
        .expect("submit")
    else {
        panic!("unexpected response");
    };

    let failed = (0..100)
        .find_map(|_| {
            let Response::Status { job: Some(current) } = client
                .request(Request::Status {
                    job_id: job.id.clone(),
                })
                .ok()?
            else {
                return None;
            };
            if current.state == JobState::Failed {
                Some(current)
            } else {
                thread::sleep(Duration::from_millis(20));
                None
            }
        })
        .expect("failed job");
    assert!(failed.exit_code.is_none());
    assert!(!failed.stderr.is_empty());

    handle.shutdown();
    server.join().expect("join").expect("server");
    let persisted = fs::read_to_string(&paths.state).expect("state");
    assert!(persisted.contains(&job.id));
    assert!(persisted.contains("failed"));
}

#[test]
fn daemon_spawn_recovers_orphaned_job_and_rejects_second_owner() {
    let directory = tempfile::tempdir().expect("tempdir");
    let paths = DaemonPaths::for_repo(directory.path());
    fs::create_dir_all(&paths.directory).expect("daemon directory");
    let job = JobRecord {
        id: "job-orphan".into(),
        program: "sh".into(),
        args: Vec::new(),
        state: JobState::Running,
        created_at: OffsetDateTime::now_utc(),
        started_at: Some(OffsetDateTime::now_utc()),
        finished_at: None,
        exit_code: None,
        stdout: String::new(),
        stderr: String::new(),
    };
    fs::write(
        &paths.state,
        serde_json::to_vec_pretty(&std::collections::BTreeMap::from([(job.id.clone(), job)]))
            .expect("serialize state"),
    )
    .expect("write state");

    let (handle, server) = spawn(paths.clone()).expect("spawn daemon");
    wait_for_socket(&paths.socket);
    let client = DaemonClient::new(&paths.socket);
    let Response::Jobs { jobs } = client.request(Request::List).expect("list") else {
        panic!("unexpected response");
    };
    assert_eq!(jobs[0].state, JobState::Interrupted);
    assert!(jobs[0].stderr.contains("daemon restarted"));

    let (_second_handle, second_server) = spawn(paths.clone()).expect("spawn second thread");
    let second_error = second_server
        .join()
        .expect("join second")
        .expect_err("ownership must be exclusive");
    assert!(second_error.message.contains("ownership unavailable"));

    handle.shutdown();
    server.join().expect("join").expect("server");
}
