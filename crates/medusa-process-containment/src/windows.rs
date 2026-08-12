use std::{io, mem::size_of, os::windows::io::AsRawHandle, process::Child};

use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, STILL_ACTIVE},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
            QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
        },
        Threading::{
            GetExitCodeProcess, OpenProcess, OpenThread, PROCESS_QUERY_LIMITED_INFORMATION,
            ResumeThread, THREAD_SUSPEND_RESUME,
        },
    },
};

/// Owns one Job Object configured to terminate its complete process tree on close.
#[derive(Debug)]
pub struct WindowsJob {
    handle: HANDLE,
}

// A Job Object handle is safe to move and share; access remains serialized by the daemon mutex.
unsafe impl Send for WindowsJob {}
unsafe impl Sync for WindowsJob {}

impl WindowsJob {
    /// Creates the job and assigns a suspended child before any user code can run.
    pub fn assign(child: &Child) -> io::Result<Self> {
        let handle = unsafe { CreateJobObjectW(None, None) }.map_err(io::Error::other)?;
        let job = Self { handle };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                job.handle,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }
        .map_err(io::Error::other)?;
        let process = HANDLE(child.as_raw_handle());
        unsafe { AssignProcessToJobObject(job.handle, process) }.map_err(io::Error::other)?;
        Ok(job)
    }

    /// Resumes every initial thread after successful Job Object assignment.
    pub fn resume(&self, child: &Child) -> io::Result<()> {
        let snapshot =
            unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }.map_err(io::Error::other)?;
        let snapshot = HandleGuard(snapshot);
        let mut entry = THREADENTRY32 {
            dwSize: size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        let pid = child.id();
        let mut found = false;
        let mut next = unsafe { Thread32First(snapshot.0, &mut entry) };
        while next.is_ok() {
            if entry.th32OwnerProcessID == pid {
                let thread =
                    unsafe { OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID) }
                        .map_err(io::Error::other)?;
                let thread = HandleGuard(thread);
                if unsafe { ResumeThread(thread.0) } == u32::MAX {
                    return Err(io::Error::last_os_error());
                }
                found = true;
            }
            next = unsafe { Thread32Next(snapshot.0, &mut entry) };
        }
        if found {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "suspended child did not expose a thread to resume",
            ))
        }
    }

    /// Returns true only after all processes assigned to this Job Object have exited.
    pub fn is_empty(&self) -> io::Result<bool> {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        unsafe {
            QueryInformationJobObject(
                Some(self.handle),
                JobObjectBasicAccountingInformation,
                &mut accounting as *mut _ as _,
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                None,
            )
        }
        .map_err(io::Error::other)?;
        Ok(accounting.ActiveProcesses == 0)
    }

    /// Force-terminates every process in this Job Object.
    pub fn terminate(&self) -> io::Result<()> {
        unsafe { TerminateJobObject(self.handle, 1) }.map_err(io::Error::other)
    }
}

/// Checks a process handle directly instead of invoking a PID-based shell command.
pub fn process_is_alive(pid: u32) -> bool {
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
        return false;
    };
    let handle = HandleGuard(handle);
    let mut exit_code = 0;
    unsafe { GetExitCodeProcess(handle.0, &mut exit_code) }
        .is_ok_and(|()| exit_code == STILL_ACTIVE.0 as u32)
}

impl Drop for WindowsJob {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

struct HandleGuard(HANDLE);

impl Drop for HandleGuard {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}
