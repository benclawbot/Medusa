use std::{collections::BTreeMap, fs, thread, time::Duration};

use medusa_daemon::{DaemonClient, DaemonPaths, JobRecord, JobState, Request, Response, spawn};

fn job(id: String) -> JobRecord {
    JobRecord {
        id,
        program: "fixture".into(),
        args: vec!["argument".into()],
        state: JobState::Succeeded,
        created_at: serde_json::from_value(serde_json::json!([2026, 276, 0, 0, 0, 0, 0, 0, 0]))
            .expect("timestamp"),
        started_at: None,
        finished_at: None,
        exit_code: Some(0),
        stdout: String::new(),
        stderr: String::new(),
    }
}

fn query(
    jobs: BTreeMap<String, JobRecord>,
    requests: &[Request],
) -> Vec<medusa_core::MedusaResult<Response>> {
    let directory = tempfile::tempdir().expect("workspace");
    let paths = DaemonPaths::for_repo(directory.path());
    fs::create_dir_all(&paths.directory).expect("daemon directory");
    fs::write(
        &paths.state,
        serde_json::to_vec(&jobs).expect("serialize jobs"),
    )
    .expect("write jobs");
    let (handle, server) = spawn(paths.clone()).expect("daemon");
    for _ in 0..100 {
        if paths.socket.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let client = DaemonClient::new(&paths.socket);
    let responses = requests
        .iter()
        .cloned()
        .map(|request| client.request(request))
        .collect();
    handle.shutdown();
    server.join().expect("join").expect("daemon result");
    let stored: BTreeMap<String, JobRecord> =
        serde_json::from_slice(&fs::read(&paths.state).expect("persisted state"))
            .expect("persisted jobs");
    assert_eq!(stored, jobs, "read-only queries preserve durable history");
    responses
}

#[test]
fn list_large_output_history_returns_summaries_without_losing_stored_output() {
    let jobs: BTreeMap<_, _> = (0..17)
        .map(|i| {
            let mut record = job(format!("job-{i:03}"));
            record.stdout = "x".repeat(1024 * 1024);
            record.stderr = "y".repeat(1024 * 1024);
            (record.id.clone(), record)
        })
        .collect();
    let mut responses = query(
        jobs,
        &[
            Request::List,
            Request::Status {
                job_id: "job-000".into(),
            },
        ],
    )
    .into_iter();
    let Response::Jobs { jobs: summaries } = responses
        .next()
        .expect("list response")
        .expect("bounded list response")
    else {
        panic!("expected jobs");
    };
    assert_eq!(summaries.len(), 17);
    assert!(
        summaries
            .iter()
            .all(|job| job.stdout.is_empty() && job.stderr.is_empty())
    );
    let Response::Status { job: Some(record) } =
        responses.next().expect("status response").expect("status")
    else {
        panic!("expected job status");
    };
    assert_eq!(record.stdout, "x".repeat(1024 * 1024));
    assert_eq!(record.stderr, "y".repeat(1024 * 1024));
    assert_eq!(record.args, vec!["argument"]);
}

#[test]
fn list_history_preserves_every_discoverable_job_and_exact_count() {
    let mut jobs: BTreeMap<_, _> = (0..160)
        .map(|i| {
            let record = job(format!("job-{i:03}"));
            (record.id.clone(), record)
        })
        .collect();
    let Response::Jobs { jobs: summaries } = query(std::mem::take(&mut jobs), &[Request::List])
        .into_iter()
        .next()
        .expect("response")
        .expect("list")
    else {
        panic!("expected jobs");
    };
    assert_eq!(summaries.len(), 160);
    assert_eq!(summaries.first().expect("first job").id, "job-000");
    assert_eq!(summaries.last().expect("last job").id, "job-159");
}
