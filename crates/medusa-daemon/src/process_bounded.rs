use std::{
    collections::BTreeMap,
    io::Read,
    path::Path,
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
#[cfg(windows)]
use medusa_process_containment::WindowsJob;
use medusa_process_containment::{ProcessOwnershipReceipt, ProcessOwnershipVerification};

const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(20);
const TERMINATION_GRACE: Duration = Duration::from_secs(1);
const MAX_STREAM_OUTPUT_BYTES: usize = 1024 * 1024;

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
        _output_dir: &Path,
    ) -> MedusaResult<Option<ProcessResult>> {
        let Some(control) = self.control(job_id)? else {
            return Err(process_error(format!(
                "daemon process control is missing for {job_id}"
            )));
        };
        if control.cancelled.load(Ordering::SeqCst) {
            return Ok(None);
        }
        control.run(program, args, current_dir).map(Some)
    }

    fn control(&self, job_id: &str) -> MedusaResult<Option<Arc<ProcessControl>>> {
        Ok(lock_controls(&self.controls)?.get(job_id).cloned())
    }
}

#[derive(Default)]
struct ProcessControl {
    cancelled: AtomicBool,
    child: Mutex<Option<Child>>,
    ownership: Mutex<Option<ProcessOwnershipReceipt>>,
    #[cfg(windows)]
    job: Mutex<Option<WindowsJob>>,
}

impl ProcessControl {
    fn run(
        &self,
        program: &str,
        args: &[String],
        current_dir: &Path,
    ) -> MedusaResult<ProcessResult> {
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(current_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_process_group(&mut command);
        let mut child = command.spawn().map_err(|error| {
            MedusaError::new(
                ErrorCode::ToolExecutionFailed,
                ErrorCategory::Execution,
                format!("failed to spawn daemon job process {program}: {error}"),
            )
        })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| process_error("daemon child stdout pipe is missing"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| process_error("daemon child stderr pipe is missing"))?;
        let stdout_reader = spawn_capture(stdout, "stdout")?;
        let stderr_reader = spawn_capture(stderr, "stderr")?;

        #[cfg(windows)]
        let job = match WindowsJob::assign(&child).and_then(|job| {
            job.resume(&child)?;
            Ok(job)
        }) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = join_capture(stdout_reader, "stdout");
                let _ = join_capture(stderr_reader, "stderr");
                return Err(MedusaError::new(
                    ErrorCode::ToolExecutionFailed,
                    ErrorCategory::Execution,
                    format!("failed to contain daemon job process {program}: {error}"),
                ));
            }
        };

        let ownership = match ProcessOwnershipReceipt::capture(child.id()) {
            Ok(ownership) => ownership,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = join_capture(stdout_reader, "stdout");
                let _ = join_capture(stderr_reader, "stderr");
                return Err(MedusaError::new(
                    ErrorCode::ToolExecutionFailed,
                    ErrorCategory::Execution,
                    format!(
                        "failed to capture ownership identity for daemon job process {program}: {error}"
                    ),
                ));
            }
        };

        {
            let mut child_slot = lock_child(&self.child)?;
            let mut ownership_slot = lock_ownership(&self.ownership)?;
            #[cfg(windows)]
            let mut job_slot = lock_job(&self.job)?;
            *child_slot = Some(child);
            *ownership_slot = Some(ownership);
            #[cfg(windows)]
            {
                *job_slot = Some(job);
            }
        }

        if self.cancelled.load(Ordering::SeqCst) {
            self.terminate()?;
        }

        let status = loop {
            let status = {
                let mut child = lock_child(&self.child)?;
                let Some(process) = child.as_mut() else {
                    return Err(process_error(
                        "daemon child process disappeared before wait",
                    ));
                };
                try_wait_preserving_unix_group_identity(process)?
            };
            if let Some(status) = status
                && self.process_tree_exited()?
            {
                break status;
            }
            thread::sleep(PROCESS_POLL_INTERVAL);
        };

        let stdout = join_capture(stdout_reader, "stdout")?;
        let stderr = join_capture(stderr_reader, "stderr")?;
        let result = ProcessResult {
            status,
            stdout,
            stderr,
            cancelled: self.cancelled.load(Ordering::SeqCst),
        };
        *lock_child(&self.child)? = None;
        *lock_ownership(&self.ownership)? = None;
        #[cfg(windows)]
        {
            *lock_job(&self.job)? = None;
        }
        Ok(result)
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
        let ownership = lock_ownership(&self.ownership)?
            .clone()
            .ok_or_else(|| process_error("daemon process ownership receipt is missing"))?;
        if ownership.pid != process.id() {
            return Err(process_error(format!(
                "daemon process ownership receipt PID {} does not match child PID {}",
                ownership.pid,
                process.id()
            )));
        }
        #[cfg(unix)]
        {
            terminate_process_tree(process, &ownership)
        }
        #[cfg(windows)]
        {
            let pid = process.id();
            let job = lock_job(&self.job)?;
            let Some(job) = job.as_ref() else {
                return Err(process_error(format!(
                    "Windows Job Object is missing for daemon process tree {pid}"
                )));
            };
            terminate_process_tree(process, job, &ownership)
        }
    }

    fn process_tree_exited(&self) -> MedusaResult<bool> {
        #[cfg(unix)]
        {
            let child = lock_child(&self.child)?;
            let Some(process) = child.as_ref() else {
                return Ok(true);
            };
            Ok(!process_group_alive(process.id()))
        }
        #[cfg(windows)]
        {
            let job = lock_job(&self.job)?;
            job.as_ref()
                .map_or(Ok(true), |job| job.is_empty().map_err(Into::into))
        }
    }
}

fn spawn_capture<R>(
    reader: R,
    stream: &'static str,
) -> MedusaResult<thread::JoinHandle<std::io::Result<Vec<u8>>>>
where
    R: Read + Send + 'static,
{
    thread::Builder::new()
        .name(format!("medusa-daemon-{stream}-capture"))
        .spawn(move || read_bounded(reader, stream))
        .map_err(|error| {
            MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Environment,
                format!("failed to spawn daemon {stream} capture thread: {error}"),
            )
        })
}

fn join_capture(
    handle: thread::JoinHandle<std::io::Result<Vec<u8>>>,
    stream: &str,
) -> MedusaResult<Vec<u8>> {
    handle
        .join()
        .map_err(|_| process_error(format!("daemon {stream} capture thread panicked")))?
        .map_err(Into::into)
}

fn read_bounded<R: Read>(mut reader: R, stream: &str) -> std::io::Result<Vec<u8>> {
    let marker = format!("\n[medusa: {stream} truncated at {MAX_STREAM_OUTPUT_BYTES} bytes]\n");
    debug_assert!(marker.len() < MAX_STREAM_OUTPUT_BYTES);
    let retained_limit = MAX_STREAM_OUTPUT_BYTES - marker.len();
    let mut output = Vec::with_capacity(MAX_STREAM_OUTPUT_BYTES);
    let mut truncated = false;
    let mut chunk = [0_u8; 8192];
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        if !truncated {
            let remaining = MAX_STREAM_OUTPUT_BYTES.saturating_sub(output.len());
            let retained = remaining.min(read);
            output.extend_from_slice(&chunk[..retained]);
            if retained < read || output.len() == MAX_STREAM_OUTPUT_BYTES {
                truncated = true;
                output.truncate(retained_limit);
                output.extend_from_slice(marker.as_bytes());
            }
        }
    }
    Ok(output)
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

fn lock_ownership(
    ownership: &Mutex<Option<ProcessOwnershipReceipt>>,
) -> MedusaResult<MutexGuard<'_, Option<ProcessOwnershipReceipt>>> {
    ownership
        .lock()
        .map_err(|_| process_error("daemon process ownership lock was poisoned"))
}

#[cfg(windows)]
fn lock_job(job: &Mutex<Option<WindowsJob>>) -> MedusaResult<MutexGuard<'_, Option<WindowsJob>>> {
    job.lock()
        .map_err(|_| process_error("daemon Windows Job Object lock was poisoned"))
}

fn process_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        message,
    )
}

fn ownership_error(
    pid: u32,
    verification: ProcessOwnershipVerification,
    action: &str,
) -> MedusaError {
    MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Execution,
        format!("refusing to {action} process tree {pid}: ownership identity is {verification:?}"),
    )
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    command.process_group(0);
}

#[cfg(windows)]
fn configure_process_group(command: &mut Command) {
    const CREATE_SUSPENDED: u32 = 0x0000_0004;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
}

#[cfg(target_os = "linux")]
fn try_wait_preserving_unix_group_identity(
    process: &mut Child,
) -> MedusaResult<Option<ExitStatus>> {
    let pid = process.id();
    match linux_process_state(pid) {
        Some(state) if !matches!(state, 'Z' | 'X' | 'x') => Ok(None),
        Some(_) if process_group_alive(pid) => Ok(None),
        Some(_) | None => Ok(process.try_wait()?),
    }
}

#[cfg(target_os = "linux")]
fn linux_process_state(pid: u32) -> Option<char> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let command_end = stat.rfind(')')?;
    stat[command_end + 1..]
        .split_whitespace()
        .next()?
        .chars()
        .next()
}

#[cfg(not(target_os = "linux"))]
fn try_wait_preserving_unix_group_identity(
    process: &mut Child,
) -> MedusaResult<Option<ExitStatus>> {
    Ok(process.try_wait()?)
}

#[cfg(unix)]
fn terminate_process_tree(
    process: &mut Child,
    ownership: &ProcessOwnershipReceipt,
) -> MedusaResult<()> {
    let pid = process.id();
    if !process_group_alive(pid) {
        let _ = process.try_wait()?;
        return Ok(());
    }
    require_current_ownership(ownership, "signal")?;
    send_group_signal("-TERM", pid)?;
    if wait_for_unix_group_exit(pid, TERMINATION_GRACE) {
        let _ = process.try_wait()?;
        return Ok(());
    }
    require_current_ownership(ownership, "force-terminate")?;
    send_group_signal("-KILL", pid)?;
    if wait_for_unix_group_exit(pid, TERMINATION_GRACE) {
        let _ = process.try_wait()?;
        return Ok(());
    }
    let _ = process.try_wait()?;
    if !process_group_alive(pid) {
        return Ok(());
    }
    Err(MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Execution,
        format!("process group {pid} remained alive after TERM/KILL escalation"),
    ))
}

#[cfg(unix)]
fn require_current_ownership(
    ownership: &ProcessOwnershipReceipt,
    action: &str,
) -> MedusaResult<()> {
    let verification = ownership.verify();
    if verification.permits_destructive_action() {
        Ok(())
    } else {
        Err(ownership_error(ownership.pid, verification, action))
    }
}

#[cfg(unix)]
fn wait_for_unix_group_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if !process_group_alive(pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(PROCESS_POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn send_group_signal(signal: &str, pid: u32) -> MedusaResult<()> {
    let group = format!("-{pid}");
    let mut command = Command::new("kill");
    command.arg(signal);
    #[cfg(target_os = "linux")]
    command.arg("--");
    let output = command
        .arg(&group)
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

#[cfg(target_os = "linux")]
fn process_group_alive(pid: u32) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return process_group_signal_alive(pid);
    };
    entries.filter_map(Result::ok).any(|entry| {
        entry.file_name().to_string_lossy().parse::<u32>().is_ok()
            && linux_process_is_live_group_member(&entry.path().join("stat"), pid)
    })
}

#[cfg(target_os = "linux")]
fn linux_process_is_live_group_member(stat_path: &Path, group_id: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(stat_path) else {
        return false;
    };
    let Some(command_end) = stat.rfind(')') else {
        return false;
    };
    let mut fields = stat[command_end + 1..].split_whitespace();
    let Some(state) = fields.next() else {
        return false;
    };
    let Some(_parent_pid) = fields.next() else {
        return false;
    };
    let Some(process_group) = fields.next() else {
        return false;
    };
    process_group.parse::<u32>() == Ok(group_id) && !matches!(state, "Z" | "X" | "x")
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_group_alive(pid: u32) -> bool {
    process_group_signal_alive(pid)
}

#[cfg(unix)]
fn process_group_signal_alive(pid: u32) -> bool {
    let group = format!("-{pid}");
    let mut command = Command::new("kill");
    command.arg("-0");
    #[cfg(target_os = "linux")]
    command.arg("--");
    command
        .arg(&group)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(windows)]
fn terminate_process_tree(
    process: &mut Child,
    job: &WindowsJob,
    ownership: &ProcessOwnershipReceipt,
) -> MedusaResult<()> {
    let pid = process.id();
    let leader_exited = process.try_wait()?.is_some();
    if leader_exited && job.is_empty().map_err(MedusaError::from)? {
        return Ok(());
    }
    if !leader_exited {
        let verification = ownership.verify();
        if !verification.permits_destructive_action() {
            return Err(ownership_error(pid, verification, "terminate"));
        }
    }
    let deadline = Instant::now() + TERMINATION_GRACE;
    loop {
        if job.is_empty().map_err(MedusaError::from)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(PROCESS_POLL_INTERVAL);
    }
    job.terminate().map_err(MedusaError::from)?;
    let deadline = Instant::now() + TERMINATION_GRACE;
    loop {
        if job.is_empty().map_err(MedusaError::from)? {
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
        format!("Windows Job Object process tree {pid} remained alive after termination"),
    ))
}

#[cfg(test)]
mod bounded_output_tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn multi_megabyte_stream_is_bounded_and_marked() {
        let bytes = vec![b'x'; MAX_STREAM_OUTPUT_BYTES * 3];
        let captured = read_bounded(Cursor::new(bytes), "stdout").expect("capture");
        assert_eq!(captured.len(), MAX_STREAM_OUTPUT_BYTES);
        let text = String::from_utf8_lossy(&captured);
        assert!(text.contains("[medusa: stdout truncated at 1048576 bytes]"));
    }

    #[cfg(unix)]
    #[test]
    fn noisy_process_output_is_bounded() {
        let directory = tempfile::tempdir().expect("tempdir");
        let registry = ProcessRegistry::default();
        registry.register("noisy").expect("register");
        let result = registry
            .run(
                "noisy",
                "sh",
                &[
                    "-c".to_owned(),
                    "yes x | head -c 3145728; yes y | head -c 3145728 >&2".to_owned(),
                ],
                directory.path(),
                directory.path(),
            )
            .expect("run")
            .expect("result");
        assert_eq!(result.stdout.len(), MAX_STREAM_OUTPUT_BYTES);
        assert_eq!(result.stderr.len(), MAX_STREAM_OUTPUT_BYTES);
        assert!(String::from_utf8_lossy(&result.stdout).contains("stdout truncated"));
        assert!(String::from_utf8_lossy(&result.stderr).contains("stderr truncated"));
    }

    #[cfg(unix)]
    #[test]
    fn infinite_output_process_can_be_cancelled_cleanly() {
        let directory = tempfile::tempdir().expect("tempdir");
        let registry = Arc::new(ProcessRegistry::default());
        registry.register("infinite").expect("register");
        let worker_registry = Arc::clone(&registry);
        let repo = directory.path().to_path_buf();
        let worker = thread::spawn(move || {
            worker_registry.run(
                "infinite",
                "sh",
                &["-c".to_owned(), "yes output".to_owned()],
                &repo,
                &repo,
            )
        });
        thread::sleep(Duration::from_millis(100));
        assert!(registry.cancel("infinite").expect("cancel"));
        let result = worker
            .join()
            .expect("worker join")
            .expect("run")
            .expect("result");
        assert!(result.cancelled);
        assert!(result.stdout.len() <= MAX_STREAM_OUTPUT_BYTES);
        assert!(String::from_utf8_lossy(&result.stdout).contains("stdout truncated"));
    }
}
