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
            });
        }
        JobState::Queued | JobState::Running => {}
    }

    let removed_from_queue = scheduler.cancel(job_id);
    match processes.cancel(job_id) {
        Ok(true) => {}
        Ok(false) => {
            return Ok(Response::Error {
                code: "job_not_cancellable".into(),
                message: format!("daemon job {job_id} no longer has an active process control"),
            });
        }
        Err(error) => {
            return Ok(Response::Error {
                code: "cancellation_failed".into(),
                message: error.to_string(),
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
