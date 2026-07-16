#![cfg(unix)]

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    thread,
    time::Duration,
};

use medusa_daemon::{
    DAEMON_PROTOCOL_VERSION, DaemonClient, DaemonPaths, JobState, Request, RequestEnvelope,
    Response, ResponseEnvelope, spawn,
};

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
fn daemon_protocol_covers_ping_list_status_submit_and_shutdown() {
    let directory = tempfile::tempdir().expect("tempdir");
    let paths = DaemonPaths::for_repo(directory.path());
    let (handle, server) = spawn(paths.clone()).expect("spawn daemon");
    wait_for_socket(&paths.socket);

    let client = DaemonClient::new(&paths.socket);
    assert_eq!(client.request(Request::Ping).expect("ping"), Response::Pong);
    assert_eq!(
        client.request(Request::List).expect("initial list"),
        Response::Jobs { jobs: Vec::new() }
    );
    assert_eq!(
        client
            .request(Request::Status {
                job_id: "missing".into(),
            })
            .expect("missing status"),
        Response::Status { job: None }
    );

    let Response::Submitted { job } = client
        .request(Request::Submit {
            program: "sh".into(),
            args: vec!["-c".into(), "printf daemon-ok".into()],
        })
        .expect("submit")
    else {
        panic!("unexpected submit response");
    };

    let completed = (0..100)
        .find_map(|_| {
            let Response::Status { job: Some(current) } = client
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
        })
        .expect("job completion");
    assert_eq!(completed.state, JobState::Succeeded);
    assert_eq!(completed.exit_code, Some(0));
    assert_eq!(completed.stdout, "daemon-ok");

    let Response::Jobs { jobs } = client.request(Request::List).expect("list jobs") else {
        panic!("unexpected list response");
    };
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].id, job.id);

    for program in ["", "rm", "sudo", "shutdown", "reboot", "mkfs"] {
        let error = client
            .request(Request::Submit {
                program: program.into(),
                args: Vec::new(),
            })
            .expect_err("denied program");
        assert!(error.to_string().contains("daemon denied program"));
    }

    assert_eq!(
        client.request(Request::Shutdown).expect("shutdown"),
        Response::Ack
    );
    server.join().expect("join daemon").expect("daemon result");
    handle.shutdown();
    assert!(!paths.socket.exists());
    assert!(paths.state.exists());
    assert!(!paths.owner.exists());
}

#[test]
fn daemon_returns_structured_error_for_incompatible_protocol() {
    let directory = tempfile::tempdir().expect("tempdir");
    let paths = DaemonPaths::for_repo(directory.path());
    let (handle, server) = spawn(paths.clone()).expect("spawn daemon");
    wait_for_socket(&paths.socket);

    let mut stream = UnixStream::connect(&paths.socket).expect("connect");
    serde_json::to_writer(
        &mut stream,
        &RequestEnvelope {
            version: DAEMON_PROTOCOL_VERSION + 1,
            request: Request::Ping,
        },
    )
    .expect("write request");
    stream.write_all(b"\n").expect("newline");
    stream.flush().expect("flush");

    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .expect("read response");
    let response: ResponseEnvelope = serde_json::from_str(&line).expect("decode response");
    assert_eq!(response.version, DAEMON_PROTOCOL_VERSION);
    assert!(matches!(
        response.response,
        Response::Error { ref code, ref message }
            if code == "incompatible_protocol" && message.contains("unsupported protocol")
    ));

    handle.shutdown();
    server.join().expect("join daemon").expect("daemon result");
}

#[test]
fn daemon_client_reports_missing_socket() {
    let directory = tempfile::tempdir().expect("tempdir");
    let client = DaemonClient::new(directory.path().join("missing.sock"));
    let error = client.request(Request::Ping).expect_err("missing socket");
    assert!(error.to_string().contains("daemon socket error"));
}
