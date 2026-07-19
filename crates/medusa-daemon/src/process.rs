use std::{
    collections::BTreeMap,
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(20);
const TERMINATION_GRACE: Duration = Duration::from_secs(1);

pub(crate) struct ProcessResult {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub cancelled: bool,
}

#[derive(Default)]
pub(crate) struct ProcessRegistry {
    controls: Mutex<BTreeMap<String, Arc<ProcessControl>>>,
}

impl ProcessRegistry {
    pub(crate) fn register(&self, job_id: &str) -> MedusaResult<()> {
        let mut controls = lock_controls(&self.controls)?;
        if controls.contains_key(job_id) {
            return Err(process_error(format!(
                "daemon process control already exists for {job_id}"
            )));
        }
        controls.insert(job_id.to_owned(), Arc::new(ProcessControl::default()));
        Ok(())
    }

    pub(crate) fn remove(&self, job_id: &str) -> MedusaResult<()> {
        lock_controls(&self.controls)?.remove(job_id);
        Ok(())
    }

    pub(crate) fn is_cancelled(&self, job_id: &str) -> MedusaResult<bool> {
        Ok(self
            .control(job_id)?
            .is_some_and(|control| control.cancelled.load(Ordering::SeqCst)))
    }

    pub(crate) fn cancel(&self, job_id: &str) -> MedusaResult<bool> {
        let Some(control) = self.control(job_id)? else {
            return Ok(false);
        };
        control.cancel()?;
        Ok(true)
    }

    pub(crate) fn cancel_all(&self) -> MedusaResult<()> {
        let controls = lock_controls(&self.controls)?
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut first_error = None;
        for control in controls {
            if let Err(error) = control.cancel()
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub(crate) fn run(
        &self,
        job_id: &str,
        program: &str,
        args: &[String],
        current_dir: &Path,
        output_dir: &Path,
    ) -> MedusaResult<Option<ProcessResult>> {
        let Some(control) = self.control(job_id)? else {
            return Err(process_error(format!(
                "daemon process control is missing for {job_id}"
            )));
        };
        if control.cancelled.load(Ordering::SeqCst) {
            return Ok(None);
        }
        control
            .run(job_id, program, args, current_dir, output_dir)
            .map(Some)
    }

    fn control(&self, job_id: &str) -> MedusaResult<Option<Arc<ProcessControl>>> {
        Ok(lock_controls(&self.controls)?.get(job_id).cloned())
    }
}

#[derive(Default)]
struct ProcessControl {
    cancelled: AtomicBool,
    child: Mutex<Option<Child>>,
}

impl ProcessControl {
    fn run(
        &self,
        job_id: &str,
        program: &str,
        args: &[String],
        current_dir: &Path,
        output_dir: &Path,
    ) -> MedusaResult<ProcessResult> {
        fs::create_dir_all(output_dir)?;
        let stdout_path = output_path(output_dir, job_id, "stdout");
        let stderr_path = output_path(output_dir, job_id, "stderr");
        let stdout = File::create(&stdout_path)?;
        let stderr = File::create(&stderr_path)?;
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(current_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        configure_process_group(&mut command);
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                cleanup_output_files(&stdout_path, &stderr_path);
                return Err(MedusaError::new(
                    ErrorCode::ToolExecutionFailed,
                    ErrorCategory::Execution,
                    format!("failed to spawn daemon job process {program}: {error}"),
                ));
            }
        };
        *lock_child(&self.child)? = Some(child);

        if self.cancelled.load(Ordering::SeqCst) {
            self.terminate()?;
        }

        let status = loop {
            let status = {
                let mut child = lock_child(&self.child)?;
                let Some(process) = child.as_mut() else {
                    cleanup_output_files(&stdout_path, &stderr_path);
                    return Err(process_error(
                        "daemon child process disappeared before wait",
                    ));
                };
                process.try_wait()?
            };
            if let Some(status) = status {
                *lock_child(&self.child)? = None;
                break status;
            }
            thread::sleep(PROCESS_POLL_INTERVAL);
        };

        let stdout = fs::read(&stdout_path);
        let stderr = fs::read(&stderr_path);
        cleanup_output_files(&stdout_path, &stderr_path);
        Ok(ProcessResult {
            status,
            stdout: stdout?,
            stderr: stderr?,
            cancelled: self.cancelled.load(Ordering::SeqCst),
        })
    }

    fn cancel(&self) -> MedusaResult<()> {
        self.cancelled.store(true, Ordering::SeqCst);
        self.terminate()
    }

    fn terminate(&self) -> MedusaResult<()> {
        let mut child = lock_child(&self.child)?;
        let Some(process) = child.as_mut() else {
            return Ok(());
        };
        let pid = process.id();
        if let Err(error) = terminate_process_tree(process) {
            let fallback = process.kill();
            return match fallback {
                Ok(()) => Err(error),
                Err(fallback_error) => Err(MedusaError::new(
                    ErrorCode::ToolExecutionFailed,
                    ErrorCategory::Execution,
                    format!(
                        "failed to terminate process tree {pid}: {error}; immediate-child fallback also failed: {fallback_error}"
                    ),
                )),
            };
        }
        Ok(())
    }
}

fn output_path(directory: &Path, job_id: &str, stream: &str) -> PathBuf {
    directory.join(format!("{job_id}.{stream}.tmp"))
}

fn cleanup_output_files(stdout: &Path, stderr: &Path) {
    let _ = fs::remove_file(stdout);
    let _ = fs::remove_file(stderr);
}

fn lock_controls(
    controls: &Mutex<BTreeMap<String, Arc<ProcessControl>>>,
) -> MedusaResult<MutexGuard<'_, BTreeMap<String, Arc<ProcessControl>>>> {
    controls
        .lock()
        .map_err(|_| process_error("daemon process registry lock was poisoned"))
}

fn lock_child(child: &Mutex<Option<Child>>) -> MedusaResult<MutexGuard<'_, Option<Child>>> {
    child
        .lock()
        .map_err(|_| process_error("daemon child process lock was poisoned"))
}

fn process_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        message,
    )
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    command.process_group(0);
}

#[cfg(windows)]
fn configure_process_group(command: &mut Command) {
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}

#[cfg(unix)]
fn terminate_process_tree(process: &mut Child) -> MedusaResult<()> {
    let pid = process.id();
    if wait_for_unix_tree_exit(process, pid, Duration::ZERO)? {
        return Ok(());
    }
    send_group_signal("-TERM", pid)?;
    if wait_for_unix_tree_exit(process, pid, TERMINATION_GRACE)? {
        return Ok(());
    }
    send_group_signal("-KILL", pid)?;
    if wait_for_unix_tree_exit(process, pid, TERMINATION_GRACE)? {
        return Ok(());
    }
    Err(MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Execution,
        format!("process group {pid} remained alive after TERM/KILL escalation"),
    ))
}

#[cfg(unix)]
fn wait_for_unix_tree_exit(
    process: &mut Child,
    pid: u32,
    timeout: Duration,
) -> MedusaResult<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        let leader_exited = process.try_wait()?.is_some();
        if leader_exited && !process_group_alive(pid) {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(PROCESS_POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn send_group_signal(signal: &str, pid: u32) -> MedusaResult<()> {
    let group = format!("-{pid}");
    let output = Command::new("kill")
        .args([signal, group.as_str()])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if output.status.success() || !process_group_alive(pid) {
        return Ok(());
    }
    Err(MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Execution,
        format!(
            "failed to send {signal} to process group {pid}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    ))
}

#[cfg(unix)]
fn process_group_alive(pid: u32) -> bool {
    let group = format!("-{pid}");
    Command::new("kill")
        .args(["-0", group.as_str()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(windows)]
fn terminate_process_tree(process: &mut Child) -> MedusaResult<()> {
    let pid = process.id();
    if process.try_wait()?.is_some() || !process_alive(pid) {
        return Ok(());
    }
    let pid_text = pid.to_string();
    let output = Command::new("taskkill")
        .args(["/PID", pid_text.as_str(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    let deadline = Instant::now() + TERMINATION_GRACE;
    loop {
        if process.try_wait()?.is_some() || !process_alive(pid) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(PROCESS_POLL_INTERVAL);
    }
    Err(MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Execution,
        format!(
            "failed to terminate Windows process tree {pid}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    ))
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    let filter = format!("PID eq {pid}");
    Command::new("tasklist")
        .args(["/FI", filter.as_str(), "/FO", "CSV", "/NH"])
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
        })
}
