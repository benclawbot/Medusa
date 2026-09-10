use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use time::OffsetDateTime;

use crate::{
    paths::DaemonPaths,
    process::ProcessRegistry,
    protocol::{JobRecord, JobState, Response},
    scheduler::JobScheduler,
    server::{lock_jobs, persist_jobs},
};

pub(crate) fn cancel_job(
    paths: &DaemonPaths,
    jobs: &Arc<Mutex<BTreeMap<String, JobRecord>>>,
    processes: &ProcessRegistry,
    scheduler: &JobScheduler,
    job_id: &str,
) -> MedusaResult<Response> {
    let current = lock_jobs(jobs)?.get(job_id).cloned();
    let Some(current) = current else {
        return Ok(Response::Cancelled { job: None });
    };
    match current.state {
        JobState::Interrupted => return Ok(Response::Cancelled { job: Some(current) }),
        JobState::Succeeded | JobState::Failed => {
            return Ok(Response::Error {
                code: "job_not_cancellable".into(),
                message: format!("daemon job {job_id} is already terminal"),
                category: ErrorCategory::Environment,
                retryable: false,
            });
        }
        JobState::Queued | JobState::Running => {}
    }

    let removed_from_queue = scheduler.cancel(job_id);
    match processes.cancel(job_id) {
        Ok(true) => {}
        Ok(false) => {
            // The process handle is already gone (or the job never started
            // one), but the job is still queued or running from the
            // scheduler's perspective. Mark it interrupted so cancellation
            // is durable instead of reporting a stale control error.
            let updated = mark_job_interrupted(
                paths,
                jobs,
                job_id,
                "cancelled by user request (no active process control)",
            )?;
            if removed_from_queue {
                processes.remove(job_id)?;
            }
            return Ok(Response::Cancelled { job: Some(updated) });
        }
        Err(error) => {
            return Ok(Response::Error {
                code: "cancellation_failed".into(),
                message: error.to_string(),
                category: ErrorCategory::Environment,
                retryable: false,
            });
        }
    }
    let updated = mark_job_interrupted(paths, jobs, job_id, "cancelled by user request")?;
    if removed_from_queue {
        processes.remove(job_id)?;
    }
    Ok(Response::Cancelled { job: Some(updated) })
}

pub(crate) fn cancel_all_jobs(
    paths: &DaemonPaths,
    jobs: &Arc<Mutex<BTreeMap<String, JobRecord>>>,
    processes: &ProcessRegistry,
    scheduler: &JobScheduler,
) -> MedusaResult<()> {
    let queued = scheduler.cancel_all_queued();
    let mut first_error = None;
    for job_id in queued {
        if let Err(error) = processes.cancel(&job_id) {
            retain_first_error(&mut first_error, error);
        }
        if let Err(error) = mark_job_interrupted(
            paths,
            jobs,
            &job_id,
            "cancelled by immediate daemon shutdown",
        ) {
            retain_first_error(&mut first_error, error);
        }
        if let Err(error) = processes.remove(&job_id) {
            retain_first_error(&mut first_error, error);
        }
    }
    if let Err(error) = processes.cancel_all() {
        retain_first_error(&mut first_error, error);
    }
    {
        let mut locked = lock_jobs(jobs)?;
        let mut changed = false;
        for job in locked.values_mut() {
            if matches!(job.state, JobState::Queued | JobState::Running) {
                job.state = JobState::Interrupted;
                job.finished_at = Some(OffsetDateTime::now_utc());
                append_detail(&mut job.stderr, "cancelled by immediate daemon shutdown");
                changed = true;
            }
        }
        if changed {
            persist_jobs(paths, &locked)?;
        }
    }
    first_error.map_or(Ok(()), Err)
}

pub(crate) fn mark_job_interrupted(
    paths: &DaemonPaths,
    jobs: &Arc<Mutex<BTreeMap<String, JobRecord>>>,
    job_id: &str,
    detail: &str,
) -> MedusaResult<JobRecord> {
    let mut locked = lock_jobs(jobs)?;
    let Some(job) = locked.get_mut(job_id) else {
        return Err(MedusaError::new(
            ErrorCode::InternalInvariant,
            ErrorCategory::Internal,
            format!("daemon job disappeared while being interrupted: {job_id}"),
        ));
    };
    job.state = JobState::Interrupted;
    job.finished_at = Some(OffsetDateTime::now_utc());
    append_detail(&mut job.stderr, detail);
    let updated = job.clone();
    persist_jobs(paths, &locked)?;
    Ok(updated)
}

pub(crate) fn append_detail(target: &mut String, detail: &str) {
    if target.contains(detail) {
        return;
    }
    if !target.is_empty() && !target.ends_with('\n') {
        target.push('\n');
    }
    target.push('[');
    target.push_str(detail);
    target.push(']');
}

fn retain_first_error(first_error: &mut Option<MedusaError>, error: MedusaError) {
    if first_error.is_none() {
        *first_error = Some(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::{DaemonLimits, JobScheduler};

    fn test_job(state: JobState) -> JobRecord {
        let now = OffsetDateTime::now_utc();
        JobRecord {
            id: "job-test".to_owned(),
            program: "true".to_owned(),
            args: Vec::new(),
            state,
            created_at: now,
            started_at: None,
            finished_at: None,
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    #[test]
    fn cancel_without_process_control_still_marks_job_interrupted() {
        let directory = tempfile::tempdir().expect("tempdir");
        let paths = DaemonPaths::for_repo(directory.path());
        let jobs = Arc::new(Mutex::new(BTreeMap::from([(
            "job-test".to_owned(),
            test_job(JobState::Running),
        )])));
        // No process registered for the job: the control handle is gone.
        let processes = ProcessRegistry::default();
        let runner: crate::scheduler::JobRunner = Arc::new(|_| {});
        let scheduler = JobScheduler::start(DaemonLimits::default(), runner).expect("scheduler");
        let response =
            cancel_job(&paths, &jobs, &processes, &scheduler, "job-test").expect("cancel");
        match response {
            Response::Cancelled { job: Some(job) } => {
                assert_eq!(job.state, JobState::Interrupted);
            }
            other => panic!("expected cancellation, got {other:?}"),
        }
        assert_eq!(
            jobs.lock()
                .expect("jobs")
                .get("job-test")
                .expect("job")
                .state,
            JobState::Interrupted
        );
    }
}
