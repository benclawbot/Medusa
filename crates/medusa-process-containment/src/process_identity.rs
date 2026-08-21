use std::io;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeProcessStartMarker {
    pub platform: &'static str,
    pub value: String,
    pub boot_id: Option<String>,
}

/// A containment-side ownership receipt captured at process launch.
///
/// Keeping the numeric PID and native creation marker together prevents later
/// cleanup code from treating a recycled PID as the process Medusa launched.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessOwnershipReceipt {
    pub pid: u32,
    pub start_marker: NativeProcessStartMarker,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessOwnershipVerification {
    VerifiedCurrent,
    VerifiedStale,
    IdentityUnavailable,
    ProcessMissing,
}

impl ProcessOwnershipVerification {
    #[must_use]
    pub const fn permits_destructive_action(self) -> bool {
        matches!(self, Self::VerifiedCurrent)
    }
}

impl ProcessOwnershipReceipt {
    pub fn capture(pid: u32) -> io::Result<Self> {
        let start_marker = process_start_marker(pid)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("process {pid} exited before ownership identity was captured"),
            )
        })?;
        Ok(Self { pid, start_marker })
    }

    #[must_use]
    pub fn verify(&self) -> ProcessOwnershipVerification {
        match process_start_marker(self.pid) {
            Ok(Some(observed)) => verify_observed_marker(&self.start_marker, Some(&observed)),
            Ok(None) => ProcessOwnershipVerification::ProcessMissing,
            Err(_) => ProcessOwnershipVerification::IdentityUnavailable,
        }
    }
}

fn verify_observed_marker(
    recorded: &NativeProcessStartMarker,
    observed: Option<&NativeProcessStartMarker>,
) -> ProcessOwnershipVerification {
    let Some(observed) = observed else {
        return ProcessOwnershipVerification::IdentityUnavailable;
    };
    if recorded == observed {
        ProcessOwnershipVerification::VerifiedCurrent
    } else {
        ProcessOwnershipVerification::VerifiedStale
    }
}

/// Acquires a process creation identity without invoking a shell on supported platforms.
/// `Ok(None)` means the process no longer exists; other acquisition failures remain explicit.
pub fn process_start_marker(pid: u32) -> io::Result<Option<NativeProcessStartMarker>> {
    if pid == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "process id 0 is invalid",
        ));
    }
    platform_process_start_marker(pid)
}

#[cfg(target_os = "linux")]
fn platform_process_start_marker(pid: u32) -> io::Result<Option<NativeProcessStartMarker>> {
    let path = format!("/proc/{pid}/stat");
    let stat = match std::fs::read_to_string(path) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let start_ticks = parse_linux_start_ticks(&stat)?;
    let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|value| value.trim().to_owned())
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("failed to read Linux boot id: {error}"),
            )
        })?;
    if boot_id.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Linux boot id is empty",
        ));
    }
    Ok(Some(NativeProcessStartMarker {
        platform: "linux_proc_stat_v1",
        value: start_ticks.to_string(),
        boot_id: Some(boot_id),
    }))
}

#[cfg(target_os = "linux")]
pub(crate) fn parse_linux_start_ticks(stat: &str) -> io::Result<u64> {
    // /proc/<pid>/stat field 2 is parenthesized `comm` and may itself contain spaces or ')'.
    // Find the final ')' and then count from field 3; starttime is field 22, offset 19.
    let command_end = stat.rfind(')').ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Linux proc stat has no command terminator",
        )
    })?;
    stat[command_end + 1..]
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Linux proc stat has no starttime",
            )
        })?
        .parse::<u64>()
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid Linux starttime: {error}"),
            )
        })
}

#[cfg(windows)]
fn platform_process_start_marker(pid: u32) -> io::Result<Option<NativeProcessStartMarker>> {
    use windows::Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE},
        System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };

    struct HandleGuard(HANDLE);
    impl Drop for HandleGuard {
        fn drop(&mut self) {
            // SAFETY: the handle is returned by OpenProcess and this guard owns its close.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    // SAFETY: querying an existing PID does not transfer ownership of caller memory; the returned
    // handle is immediately wrapped in HandleGuard and only used for process-time queries.
    let handle = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(handle) => HandleGuard(handle),
        Err(error) => {
            let code = error.code().0 as u32;
            if matches!(code, 0x8007_0006 | 0x8007_0057 | 0x8007_007F) {
                return Ok(None);
            }
            return Err(io::Error::other(error));
        }
    };
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: handle.0 is live for this scope and each FILETIME output points to initialized,
    // writable storage whose layout is defined by the Windows API.
    unsafe { GetProcessTimes(handle.0, &mut creation, &mut exit, &mut kernel, &mut user) }
        .map_err(io::Error::other)?;
    let creation_100ns =
        (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
    Ok(Some(NativeProcessStartMarker {
        platform: "windows_filetime_100ns_v1",
        value: creation_100ns.to_string(),
        boot_id: None,
    }))
}

#[cfg(target_os = "macos")]
fn platform_process_start_marker(pid: u32) -> io::Result<Option<NativeProcessStartMarker>> {
    use std::ffi::{c_int, c_void};

    const PROC_PIDTBSDINFO: c_int = 3;

    #[repr(C)]
    struct ProcBsdInfo {
        pbi_flags: u32,
        pbi_status: u32,
        pbi_xstatus: u32,
        pbi_pid: u32,
        pbi_ppid: u32,
        pbi_uid: u32,
        pbi_gid: u32,
        pbi_ruid: u32,
        pbi_rgid: u32,
        pbi_svuid: u32,
        pbi_svgid: u32,
        rfu_1: u32,
        pbi_comm: [i8; 16],
        pbi_name: [i8; 32],
        pbi_nfiles: u32,
        pbi_pgid: u32,
        pbi_pjobc: u32,
        e_tdev: u32,
        e_tpgid: u32,
        pbi_nice: i32,
        pbi_start_tvsec: u64,
        pbi_start_tvusec: u64,
    }

    unsafe extern "C" {
        fn proc_pidinfo(
            pid: c_int,
            flavor: c_int,
            arg: u64,
            buffer: *mut c_void,
            buffersize: c_int,
        ) -> c_int;
    }

    let mut info = std::mem::MaybeUninit::<ProcBsdInfo>::zeroed();
    let expected = std::mem::size_of::<ProcBsdInfo>();
    // SAFETY: `info` provides `expected` bytes of writable storage for PROC_PIDTBSDINFO and the
    // buffer is not read unless libproc reports the complete structure size below.
    let written = unsafe {
        proc_pidinfo(
            pid as c_int,
            PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            expected as c_int,
        )
    };
    if written == 0 {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(libc::ESRCH) => Ok(None),
            Some(libc::EPERM) => {
                // On macOS, libproc can report EPERM for a child that has already been reaped.
                // Confirm absence with the non-destructive signal-0 probe before treating the
                // ownership identity as missing; a live but inaccessible PID remains fail-closed.
                let probe = unsafe { libc::kill(pid as c_int, 0) };
                if probe == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                    Ok(None)
                } else {
                    Err(error)
                }
            }
            _ => Err(error),
        };
    }
    if written as usize != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("libproc returned {written} bytes, expected {expected}"),
        ));
    }
    // SAFETY: proc_pidinfo reported exactly the complete ProcBsdInfo size above.
    let info = unsafe { info.assume_init() };
    Ok(Some(NativeProcessStartMarker {
        platform: "macos_libproc_bsdinfo_v1",
        value: format!("{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec),
        boot_id: None,
    }))
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn platform_process_start_marker(pid: u32) -> io::Result<Option<NativeProcessStartMarker>> {
    use std::process::{Command, Stdio};

    // Explicit degraded fallback for other BSD/Unix targets. Linux and macOS never use this path.
    let output = Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if value.is_empty() {
        return Ok(None);
    }
    Ok(Some(NativeProcessStartMarker {
        platform: "unix_ps_lstart_degraded_v1",
        value,
        boot_id: None,
    }))
}

#[cfg(not(any(unix, windows)))]
fn platform_process_start_marker(_pid: u32) -> io::Result<Option<NativeProcessStartMarker>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native process identity is unsupported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker(value: &str) -> NativeProcessStartMarker {
        NativeProcessStartMarker {
            platform: "test_v1",
            value: value.to_owned(),
            boot_id: Some("boot-a".to_owned()),
        }
    }

    #[test]
    fn ownership_receipt_rejects_recycled_pid_marker() {
        assert_eq!(
            verify_observed_marker(&marker("100"), Some(&marker("200"))),
            ProcessOwnershipVerification::VerifiedStale
        );
        assert!(!ProcessOwnershipVerification::VerifiedStale.permits_destructive_action());
    }

    #[test]
    fn ownership_receipt_accepts_same_marker() {
        let recorded = marker("100");
        assert_eq!(
            verify_observed_marker(&recorded, Some(&recorded)),
            ProcessOwnershipVerification::VerifiedCurrent
        );
        assert!(ProcessOwnershipVerification::VerifiedCurrent.permits_destructive_action());
    }

    #[test]
    fn missing_observation_is_explicitly_unavailable() {
        assert_eq!(
            verify_observed_marker(&marker("100"), None),
            ProcessOwnershipVerification::IdentityUnavailable
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_parser_handles_spaces_and_parentheses_in_command() {
        let mut fields = vec!["S".to_owned()];
        fields.extend((4..22).map(|field| field.to_string()));
        fields.push("987654".to_owned());
        let stat = format!("123 (worker name) with ) parens) {}", fields.join(" "));
        assert_eq!(parse_linux_start_ticks(&stat).expect("start ticks"), 987654);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reaped_child_is_reported_missing_instead_of_identity_unavailable() {
        use std::process::Command;

        let mut child = Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn macOS child");
        let receipt = ProcessOwnershipReceipt::capture(child.id()).expect("capture child identity");
        child.kill().expect("terminate macOS child");
        child.wait().expect("reap macOS child");
        assert_eq!(
            receipt.verify(),
            ProcessOwnershipVerification::ProcessMissing,
            "a reaped child must not surface libproc EPERM as identity unavailable"
        );
    }

    #[test]
    fn current_process_has_native_start_marker() {
        let marker = process_start_marker(std::process::id())
            .expect("identity probe")
            .expect("current process marker");
        assert!(!marker.platform.is_empty());
        assert!(!marker.value.is_empty());
        #[cfg(target_os = "linux")]
        assert!(
            marker
                .boot_id
                .as_ref()
                .is_some_and(|value| !value.is_empty())
        );
    }

    #[test]
    fn current_process_receipt_verifies_before_destructive_action() {
        let receipt = ProcessOwnershipReceipt::capture(std::process::id()).expect("receipt");
        assert_eq!(
            receipt.verify(),
            ProcessOwnershipVerification::VerifiedCurrent
        );
    }
}
