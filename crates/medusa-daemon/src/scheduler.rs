use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

pub(crate) type JobRunner = Arc<dyn Fn(String) + Send + Sync + 'static>;

/// Bounded synchronous daemon worker limits.
///
/// Running jobs occupy `max_concurrent_jobs` fixed worker threads. Additional accepted jobs wait
/// in a queue capped by `max_queued_jobs`; submissions beyond that capacity receive a busy
/// response instead of creating more operating-system threads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DaemonLimits {
    pub max_concurrent_jobs: usize,
    pub max_queued_jobs: usize,
}

impl Default for DaemonLimits {
    fn default() -> Self {
        Self {
            max_concurrent_jobs: 4,
            max_queued_jobs: 32,
        }
    }
}

impl DaemonLimits {
    pub(crate) fn validate(self) -> MedusaResult<Self> {
        if self.max_concurrent_jobs == 0 {
            return Err(invalid_limit("max_concurrent_jobs", 0));
        }
        if self.max_queued_jobs == 0 {
            return Err(invalid_limit("max_queued_jobs", 0));
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SubmitError {
    Busy,
    Stopped,
}

struct SchedulerState {
    queue: VecDeque<String>,
    accepting: bool,
    stopping: bool,
}

struct Shared {
    state: Mutex<SchedulerState>,
    available: Condvar,
    runner: JobRunner,
}

pub(crate) struct JobScheduler {
    shared: Arc<Shared>,
    workers: Vec<thread::JoinHandle<()>>,
    max_queued_jobs: usize,
}

impl JobScheduler {
    pub(crate) fn start(limits: DaemonLimits, runner: JobRunner) -> MedusaResult<Self> {
        let limits = limits.validate()?;
        let shared = Arc::new(Shared {
            state: Mutex::new(SchedulerState {
                queue: VecDeque::new(),
                accepting: true,
                stopping: false,
            }),
            available: Condvar::new(),
            runner,
        });
        let mut scheduler = Self {
            shared,
            workers: Vec::with_capacity(limits.max_concurrent_jobs),
            max_queued_jobs: limits.max_queued_jobs,
        };

        for index in 0..limits.max_concurrent_jobs {
            let shared = Arc::clone(&scheduler.shared);
            match thread::Builder::new()
                .name(format!("medusa-job-worker-{index}"))
                .spawn(move || worker_loop(shared))
            {
                Ok(worker) => scheduler.workers.push(worker),
                Err(error) => {
                    let _ = scheduler.shutdown();
                    return Err(MedusaError::new(
                        ErrorCode::DependencyUnavailable,
                        ErrorCategory::Environment,
                        format!("failed to spawn daemon job worker {index}: {error}"),
                    ));
                }
            }
        }
        Ok(scheduler)
    }

    pub(crate) fn enqueue(&self, job_id: String) -> Result<(), SubmitError> {
        let mut state = lock_state(&self.shared.state);
        if !state.accepting || state.stopping {
            return Err(SubmitError::Stopped);
        }
        if state.queue.len() >= self.max_queued_jobs {
            return Err(SubmitError::Busy);
        }
        state.queue.push_back(job_id);
        self.shared.available.notify_one();
        Ok(())
    }

    pub(crate) fn shutdown(&mut self) -> MedusaResult<()> {
        if self.workers.is_empty() {
            return Ok(());
        }
        {
            let mut state = lock_state(&self.shared.state);
            state.accepting = false;
            state.stopping = true;
        }
        self.shared.available.notify_all();

        let mut first_error = None;
        for worker in self.workers.drain(..) {
            if worker.join().is_err() && first_error.is_none() {
                first_error = Some(MedusaError::new(
                    ErrorCode::InternalInvariant,
                    ErrorCategory::Internal,
                    "daemon job worker terminated unexpectedly",
                ));
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

impl Drop for JobScheduler {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn worker_loop(shared: Arc<Shared>) {
    loop {
        let job_id = {
            let mut state = lock_state(&shared.state);
            while state.queue.is_empty() && !state.stopping {
                state = wait_state(&shared.available, state);
            }
            match state.queue.pop_front() {
                Some(job_id) => job_id,
                None if state.stopping => return,
                None => continue,
            }
        };
        (shared.runner)(job_id);
    }
}

fn lock_state(mutex: &Mutex<SchedulerState>) -> MutexGuard<'_, SchedulerState> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn wait_state<'a>(
    available: &Condvar,
    state: MutexGuard<'a, SchedulerState>,
) -> MutexGuard<'a, SchedulerState> {
    available
        .wait(state)
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn invalid_limit(name: &str, value: usize) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        format!("daemon {name} must be greater than zero, got {value}"),
    )
}
