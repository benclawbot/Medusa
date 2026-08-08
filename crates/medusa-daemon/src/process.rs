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
#[cfg(windows)]
use medusa_process_containment::WindowsJob;
use medusa_process_containment::{ProcessOwnershipReceipt, ProcessOwnershipVerification};

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
    ownership: Mutex<Option<ProcessOwnershipReceipt>>,
    #[cfg(windows)]
    job: Mutex<Option<WindowsJob>>,
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
        let mut child = match command.spawn() {
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
        #[cfg(windows)]
        let job = match WindowsJob::assign(&child).and_then(|job| {
            job.resume(&child)?;
            Ok(job)
        }) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                cleanup_output_files(&stdout_path, &stderr_path);
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
                cleanup_output_files(&stdout_path, &stderr_path);
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
                    cleanup_output_files(&stdout_path, &stderr_path);
                    return Err(process_error(
                        "daemon child process disappeared before wait",
                    ));
                };
                try_wait_preserving_unix_group_identity(process)?
            };
            if let Some(status) = status {
                if self.process_tree_exited()? {
                    break status;
                }
            }
            thread::sleep(PROCESS_POLL_INTERVAL);
        };

        let result = (|| {
            let stdout = fs::read(&stdout_path)?;
            let stderr = fs::read(&stderr_path)?;
            Ok(ProcessResult {
                status,
                stdout,
                stderr,
                cancelled: self.cancelled.load(Ordering::SeqCst),
            })
        })();
        cleanup_output_files(&stdout_path, &stderr_path);
        *lock_child(&self.child)? = None;
        *lock_ownership(&self.ownership)? = None;
        #[cfg(windows)]
        {
            *lock_job(&self.job)? = None;
        }
        result
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
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
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

    // The leader has deliberately not been reaped while the group is alive, so its PID cannot be
    // recycled between TERM and KILL. Re-verify the same launch receipt before escalation anyway;
    // mismatch or probe uncertainty fails closed rather than targeting a numeric group blindly.
    require_current_ownership(ownership, "force-terminate")?;
    send_group_signal("-KILL", pid)?;
    if wait_for_unix_group_exit(pid, TERMINATION_GRACE) {
        let _ = process.try_wait()?;
        return Ok(());
    }

    // No further destructive action will be attempted. It is now safe to reap a terminated leader
    // before reporting that descendants remain.
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
    let Ok(entries) = fs::read_dir("/proc") else {
        return process_group_signal_alive(pid);
    };
    entries.filter_map(Result::ok).any(|entry| {
        entry.file_name().to_string_lossy().parse::<u32>().is_ok()
            && linux_process_is_live_group_member(&entry.path().join("stat"), pid)
    })
}

#[cfg(target_os = "linux")]
fn linux_process_is_live_group_member(stat_path: &Path, group_id: u32) -> bool {
    let Ok(stat) = fs::read_to_string(stat_path) else {
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
    // The Job Object handle is a stable kernel ownership anchor for descendants even after the
    // original leader exits, so this action cannot be redirected by numeric PID reuse.
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn spawn_sleep() -> Child {
        let mut command = Command::new("sleep");
        command
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_process_group(&mut command);
        command.spawn().expect("spawn sleep")
    }

    #[cfg(target_os = "linux")]
    fn spawn_sleep_tree() -> Child {
        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 30 & wait"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_process_group(&mut command);
        command.spawn().expect("spawn sleep tree")
    }

    #[cfg(target_os = "linux")]
    fn spawn_exiting_leader_tree() -> Child {
        let mut command = Command::new("sh");
        command
            .args(["-c", "(sleep 30) &"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_process_group(&mut command);
        command.spawn().expect("spawn exiting leader tree")
    }

    #[cfg(target_os = "linux")]
    fn linux_group_member_count(group_id: u32) -> usize {
        fs::read_dir("/proc")
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_name().to_string_lossy().parse::<u32>().is_ok()
                    && linux_process_is_live_group_member(&entry.path().join("stat"), group_id)
            })
            .count()
    }

    #[cfg(target_os = "linux")]
    fn wait_for_linux_group_members(group_id: u32, minimum: usize) -> bool {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if linux_group_member_count(group_id) >= minimum {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(PROCESS_POLL_INTERVAL);
        }
    }

    #[test]
    fn verified_real_process_is_cancellable() {
        let mut child = spawn_sleep();
        let receipt = ProcessOwnershipReceipt::capture(child.id()).expect("capture receipt");
        terminate_process_tree(&mut child, &receipt).expect("terminate verified child");
        assert!(child.try_wait().expect("wait child").is_some());
    }

    #[test]
    fn mismatched_receipt_never_signals_live_process() {
        let mut child = spawn_sleep();
        let mut receipt = ProcessOwnershipReceipt::capture(child.id()).expect("capture receipt");
        receipt.start_marker.value.push_str("-recycled");

        let error = terminate_process_tree(&mut child, &receipt).expect_err("identity mismatch");
        assert!(error.to_string().contains("VerifiedStale"));
        assert!(child.try_wait().expect("child status").is_none());

        child.kill().expect("cleanup child");
        child.wait().expect("reap child");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stale_leader_identity_cannot_target_contained_grandchildren() {
        let mut child = spawn_sleep_tree();
        let receipt = ProcessOwnershipReceipt::capture(child.id()).expect("capture receipt");
        assert!(
            wait_for_linux_group_members(child.id(), 2),
            "shell leader never spawned its contained child"
        );

        let mut stale_receipt = receipt.clone();
        stale_receipt.start_marker.value.push_str("-recycled");
        let error = terminate_process_tree(&mut child, &stale_receipt)
            .expect_err("stale leader identity must fail closed");
        assert!(error.to_string().contains("VerifiedStale"));
        assert!(
            linux_group_member_count(child.id()) >= 2,
            "stale identity unexpectedly signalled the contained process tree"
        );

        terminate_process_tree(&mut child, &receipt).expect("cleanup verified process tree");
        assert!(!process_group_alive(child.id()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn exited_leader_remains_identity_anchor_while_descendant_runs() {
        let mut child = spawn_exiting_leader_tree();
        let receipt = ProcessOwnershipReceipt::capture(child.id()).expect("capture receipt");
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            assert!(
                try_wait_preserving_unix_group_identity(&mut child)
                    .expect("preserve leader")
                    .is_none(),
                "leader was reaped while its process group still had a live descendant"
            );
            if linux_process_state(child.id()).is_some_and(|state| matches!(state, 'Z' | 'X' | 'x'))
            {
                break;
            }
            assert!(Instant::now() < deadline, "leader did not exit in time");
            thread::sleep(PROCESS_POLL_INTERVAL);
        }
        assert_eq!(receipt.verify(), ProcessOwnershipVerification::VerifiedCurrent);
        terminate_process_tree(&mut child, &receipt).expect("cleanup verified process tree");
        assert!(!process_group_alive(child.id()));
    }
}
