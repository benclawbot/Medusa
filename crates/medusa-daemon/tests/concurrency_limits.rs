use std::{
    collections::BTreeMap,
    sync::{Arc, Barrier},
    thread,
    time::Duration,
};

use medusa_daemon::{
    DaemonClient, DaemonLimits, DaemonPaths, JobRecord, JobState, Request, Response,
    spawn_with_limits,
};

const CONCURRENT_PING_CLIENTS: usize = 64;

fn wait_for_endpoint(path: &std::path::Path) {
    for _ in 0..250 {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("daemon endpoint did not appear: {}", path.display());
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
    panic!("job {job_id} did not reach {expected:?}");
}

#[cfg(unix)]
fn delayed_command(label: &str, milliseconds: u64) -> (String, Vec<String>) {
    let seconds = milliseconds / 1_000;
    let remainder = milliseconds % 1_000;
    (
        "sh".to_owned(),
        vec![
            "-c".to_owned(),
            format!("sleep {seconds}.{remainder:03}; printf {label}"),
        ],
    )
}

#[cfg(windows)]
fn delayed_command(label: &str, milliseconds: u64) -> (String, Vec<String>) {
    (
        "powershell.exe".to_owned(),
        vec![
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-Command".to_owned(),
            format!("Start-Sleep -Milliseconds {milliseconds}; [Console]::Write('{label}')"),
        ],
    )
}

#[test]
fn sixty_four_concurrent_ping_clients_complete_without_async_runtime() {
    let directory = tempfile::tempdir().expect("tempdir");
    let paths = DaemonPaths::for_repo(directory.path());
    let (handle, server) = spawn_with_limits(paths.clone(), DaemonLimits::default())
        .expect("spawn bounded daemon");
    wait_for_endpoint(&paths.socket);

    let barrier = Arc::new(Barrier::new(CONCURRENT_PING_CLIENTS + 1));
    let clients = (0..CONCURRENT_PING_CLIENTS)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            let client = DaemonClient::new(&paths.socket);
            thread::spawn(move || {
                barrier.wait();
                client.request(Request::Ping)
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();

    for client in clients {
        assert!(matches!(
            client.join().expect("join ping client"),
            Ok(Response::Pong)
        ));
    }

    handle.shutdown();
    server.join().expect("join daemon").expect("daemon result");
}

#[test]
fn bounded_workers_apply_backpressure_and_shutdown_drains_accepted_jobs() {
    let directory = tempfile::tempdir().expect("tempdir");
    let paths = DaemonPaths::for_repo(directory.path());
    let limits = DaemonLimits {
        max_concurrent_jobs: 1,
        max_queued_jobs: 1,
    };
    let (_handle, server) =
        spawn_with_limits(paths.clone(), limits).expect("spawn bounded daemon");
    wait_for_endpoint(&paths.socket);
    let client = DaemonClient::new(&paths.socket);

    let (program, args) = delayed_command("first", 1_000);
    let Response::Submitted { job: first } = client
        .request(Request::Submit { program, args })
        .expect("submit first")
    else {
        panic!("unexpected first submit response");
    };
    wait_for_state(&client, &first.id, JobState::Running);

    let (program, args) = delayed_command("second", 100);
    let Response::Submitted { job: second } = client
        .request(Request::Submit { program, args })
        .expect("submit second")
    else {
        panic!("unexpected second submit response");
    };

    let Response::Jobs { jobs } = client.request(Request::List).expect("list jobs") else {
        panic!("unexpected list response");
    };
    assert_eq!(
        jobs.iter()
            .filter(|job| job.state == JobState::Running)
            .count(),
        1
    );
    assert_eq!(
        jobs.iter()
            .filter(|job| job.state == JobState::Queued)
            .count(),
        1
    );

    let (program, args) = delayed_command("rejected", 100);
    let Response::Error { code, message } = client
        .request(Request::Submit { program, args })
        .expect("busy response")
    else {
        panic!("third submission must be rejected");
    };
    assert_eq!(code, "daemon_busy");
    assert!(message.contains("capacity"));

    assert!(matches!(
        client.request(Request::Shutdown).expect("shutdown request"),
        Response::Ack
    ));
    server.join().expect("join daemon").expect("daemon result");

    let persisted: BTreeMap<String, JobRecord> =
        serde_json::from_slice(&std::fs::read(&paths.state).expect("read persisted jobs"))
            .expect("decode persisted jobs");
    assert_eq!(persisted.len(), 2);
    assert_eq!(persisted[&first.id].state, JobState::Succeeded);
    assert_eq!(persisted[&first.id].stdout, "first");
    assert_eq!(persisted[&second.id].state, JobState::Succeeded);
    assert_eq!(persisted[&second.id].stdout, "second");
}
