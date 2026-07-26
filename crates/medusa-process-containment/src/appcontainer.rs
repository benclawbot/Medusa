use std::{
    ffi::{OsStr, OsString, c_void},
    io,
    mem::{size_of, zeroed},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        process::ExitStatusExt,
    },
    path::{Path, PathBuf},
    process::{ExitStatus, Output},
    ptr::{null, null_mut},
    slice,
    time::Duration,
};

use windows_sys::Win32::{
    Foundation::{
        CloseHandle, GetLastError, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, LocalFree,
        SetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Security::{
        Authorization::ConvertSidToStringSidW,
        FreeSid, PSID, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES,
        Isolation::{CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName},
    },
    System::{
        IO::ReadFile,
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
            JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        },
        Pipes::CreatePipe,
        Threading::{
            CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
            DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
            InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
            PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW,
            UpdateProcThreadAttribute, WaitForSingleObject,
        },
    },
};

const PROFILE_NAME: &str = "Medusa.CommandSandbox";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const BROKEN_PIPE: u32 = 109;

/// Effective restrictions established for a Windows command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsSandboxRestrictions {
    pub backend: &'static str,
    pub restrictions: &'static [&'static str],
}

impl Default for WindowsSandboxRestrictions {
    fn default() -> Self {
        Self {
            backend: "windows_appcontainer",
            restrictions: &[
                "app_container",
                "network_denied",
                "job_kill_on_close",
                "environment_allowlist",
                "repository_acl_scope",
            ],
        }
    }
}

/// Launches a command only after AppContainer and Job Object containment are established.
///
/// No user-supplied process is created when any setup operation fails.
pub fn run_appcontainer(repo: &Path, program: &str, args: &[String]) -> io::Result<Output> {
    let root = repo.canonicalize()?;
    let executable = resolve_program(program)?;
    let profile = AppContainerProfile::open_or_create()?;
    let sid = profile.sid_string()?;

    let root_acl = AclGrant::grant(&root, &sid, "(OI)(CI)M")?;
    let executable_acl = AclGrant::grant(&executable, &sid, "RX")?;
    let result = unsafe { launch(&root, &executable, args, profile.sid) };
    drop(executable_acl);
    drop(root_acl);
    result
}

unsafe fn launch(
    root: &Path,
    executable: &Path,
    args: &[String],
    sid: PSID,
) -> io::Result<Output> {
    let job = OwnedHandle::new(unsafe { CreateJobObjectW(null(), null()) })?;
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
        | JOB_OBJECT_LIMIT_JOB_MEMORY;
    limits.BasicLimitInformation.ActiveProcessLimit = 64;
    limits.JobMemoryLimit = 2 * 1024 * 1024 * 1024;
    if unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            &raw const limits as *const c_void,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }

    let (stdout_read, stdout_write) = create_inheritable_pipe()?;
    let (stderr_read, stderr_write) = create_inheritable_pipe()?;

    let mut attribute_bytes = 0usize;
    unsafe { InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut attribute_bytes) };
    if attribute_bytes == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut attributes = vec![0u8; attribute_bytes];
    let attribute_list = attributes.as_mut_ptr().cast();
    if unsafe { InitializeProcThreadAttributeList(attribute_list, 1, 0, &mut attribute_bytes) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let _attributes = AttributeList(attribute_list);

    let security = SECURITY_CAPABILITIES {
        AppContainerSid: sid,
        Capabilities: null_mut(),
        CapabilityCount: 0,
        Reserved: 0,
    };
    if unsafe {
        UpdateProcThreadAttribute(
            attribute_list,
            0,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
            &raw const security as *mut c_void,
            size_of::<SECURITY_CAPABILITIES>(),
            null_mut(),
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }

    let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdOutput = stdout_write.0;
    startup.StartupInfo.hStdError = stderr_write.0;
    startup.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
    startup.lpAttributeList = attribute_list;

    let mut process: PROCESS_INFORMATION = unsafe { zeroed() };
    let mut command_line = wide_command_line(executable, args);
    let mut environment = environment_block(root)?;
    let root_wide = wide_null(root.as_os_str());
    let executable_wide = wide_null(executable.as_os_str());

    if unsafe {
        CreateProcessW(
            executable_wide.as_ptr(),
            command_line.as_mut_ptr(),
            null(),
            null(),
            1,
            CREATE_SUSPENDED
                | CREATE_NO_WINDOW
                | CREATE_UNICODE_ENVIRONMENT
                | EXTENDED_STARTUPINFO_PRESENT,
            environment.as_mut_ptr().cast(),
            root_wide.as_ptr(),
            &startup.StartupInfo,
            &mut process,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let process_handle = OwnedHandle::new(process.hProcess)?;
    let thread_handle = OwnedHandle::new(process.hThread)?;

    if unsafe { AssignProcessToJobObject(job.0, process_handle.0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { ResumeThread(thread_handle.0) } == u32::MAX {
        return Err(io::Error::last_os_error());
    }
    drop(stdout_write);
    drop(stderr_write);

    let wait = unsafe { WaitForSingleObject(process_handle.0, COMMAND_TIMEOUT.as_millis() as u32) };
    if wait == WAIT_TIMEOUT {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "Windows AppContainer command timed out after {} seconds",
                COMMAND_TIMEOUT.as_secs()
            ),
        ));
    }
    if wait != WAIT_OBJECT_0 {
        return Err(io::Error::last_os_error());
    }

    let mut exit_code = 0u32;
    if unsafe { GetExitCodeProcess(process_handle.0, &mut exit_code) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let stdout = read_all(stdout_read.0)?;
    let stderr = read_all(stderr_read.0)?;
    Ok(Output {
        status: ExitStatus::from_raw(exit_code),
        stdout,
        stderr,
    })
}

fn create_inheritable_pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    let mut read: HANDLE = null_mut();
    let mut write: HANDLE = null_mut();
    if unsafe { CreatePipe(&mut read, &mut write, &mut attributes, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let read = OwnedHandle::new(read)?;
    let write = OwnedHandle::new(write)?;
    if unsafe { SetHandleInformation(read.0, HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((read, write))
}

fn resolve_program(program: &str) -> io::Result<PathBuf> {
    let requested = Path::new(program);
    if requested.is_absolute() || requested.components().count() > 1 {
        return requested.canonicalize();
    }

    let path = std::env::var_os("PATH").unwrap_or_default();
    let extensions = std::env::var_os("PATHEXT")
        .unwrap_or_else(|| OsString::from(".COM;.EXE;.BAT;.CMD"));
    let has_extension = requested.extension().is_some();
    for directory in std::env::split_paths(&path) {
        if has_extension {
            let candidate = directory.join(requested);
            if candidate.is_file() {
                return candidate.canonicalize();
            }
        } else {
            for extension in extensions.to_string_lossy().split(';') {
                let candidate = directory.join(format!("{program}{extension}"));
                if candidate.is_file() {
                    return candidate.canonicalize();
                }
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("program not found on PATH: {program}"),
    ))
}

fn environment_block(root: &Path) -> io::Result<Vec<u16>> {
    let temp = root.join(".medusa-sandbox-tmp");
    std::fs::create_dir_all(&temp)?;
    let mut values = vec![
        ("PATH", std::env::var_os("PATH").unwrap_or_default()),
        (
            "SystemRoot",
            std::env::var_os("SystemRoot").unwrap_or_default(),
        ),
        ("TEMP", temp.clone().into_os_string()),
        ("TMP", temp.into_os_string()),
        ("MEDUSA_SANDBOX", OsString::from("windows-appcontainer")),
        ("MEDUSA_NETWORK", OsString::from("disabled")),
    ];
    values.sort_by_key(|(key, _)| key.to_ascii_uppercase());
    let mut block = Vec::new();
    for (key, value) in values {
        block.extend(OsStr::new(key).encode_wide());
        block.push('=' as u16);
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

fn wide_command_line(executable: &Path, args: &[String]) -> Vec<u16> {
    let mut command = quote(executable.as_os_str());
    for arg in args {
        command.push(' ');
        command.push_str(&quote(OsStr::new(arg)));
    }
    command.encode_utf16().chain([0]).collect()
}

fn quote(value: &OsStr) -> String {
    let value = value.to_string_lossy();
    if !value.is_empty() && !value.contains([' ', '\t', '"']) {
        return value.into_owned();
    }
    let mut quoted = String::from("\"");
    let mut slashes = 0usize;
    for character in value.chars() {
        if character == '\\' {
            slashes += 1;
        } else {
            if character == '"' {
                quoted.push_str(&"\\".repeat(slashes * 2 + 1));
            } else {
                quoted.push_str(&"\\".repeat(slashes));
            }
            slashes = 0;
            quoted.push(character);
        }
    }
    quoted.push_str(&"\\".repeat(slashes * 2));
    quoted.push('"');
    quoted
}

fn wide_null(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain([0]).collect()
}

fn read_all(handle: HANDLE) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let mut read = 0u32;
        if unsafe {
            ReadFile(
                handle,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut read,
                null_mut(),
            )
        } == 0
        {
            let code = unsafe { GetLastError() };
            if code == BROKEN_PIPE {
                break;
            }
            return Err(io::Error::from_raw_os_error(code as i32));
        }
        if read == 0 {
            break;
        }
        output.extend_from_slice(&buffer[..read as usize]);
    }
    Ok(output)
}

struct AppContainerProfile {
    sid: PSID,
}

impl AppContainerProfile {
    fn open_or_create() -> io::Result<Self> {
        let name = wide_null(OsStr::new(PROFILE_NAME));
        let mut sid: PSID = null_mut();
        let mut result = unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) };
        if result < 0 {
            let display = wide_null(OsStr::new("Medusa command sandbox"));
            let description = wide_null(OsStr::new("Network-isolated Medusa command runner"));
            result = unsafe {
                CreateAppContainerProfile(
                    name.as_ptr(),
                    display.as_ptr(),
                    description.as_ptr(),
                    null(),
                    0,
                    &mut sid,
                )
            };
        }
        if result < 0 || sid.is_null() {
            return Err(io::Error::from_raw_os_error(result));
        }
        Ok(Self { sid })
    }

    fn sid_string(&self) -> io::Result<String> {
        let mut text: *mut u16 = null_mut();
        if unsafe { ConvertSidToStringSidW(self.sid, &mut text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let len = (0..).take_while(|&index| unsafe { *text.add(index) } != 0).count();
        let value = unsafe { OsString::from_wide(slice::from_raw_parts(text, len)) }
            .to_string_lossy()
            .into_owned();
        unsafe { LocalFree(text.cast()) };
        Ok(value)
    }
}

impl Drop for AppContainerProfile {
    fn drop(&mut self) {
        unsafe { FreeSid(self.sid) };
    }
}

struct AclGrant {
    path: PathBuf,
    sid: String,
}

impl AclGrant {
    fn grant(path: &Path, sid: &str, rights: &str) -> io::Result<Self> {
        let status = std::process::Command::new("icacls.exe")
            .arg(path)
            .args(["/grant", &format!("*{sid}:{rights}"), "/Q"])
            .status()?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "icacls grant failed with {status}"
            )));
        }
        Ok(Self {
            path: path.to_owned(),
            sid: sid.to_owned(),
        })
    }
}

impl Drop for AclGrant {
    fn drop(&mut self) {
        let _ = std::process::Command::new("icacls.exe")
            .arg(&self.path)
            .args(["/remove", &format!("*{}", self.sid), "/Q"])
            .status();
    }
}

struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn new(handle: HANDLE) -> io::Result<Self> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(handle))
        }
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

struct AttributeList(*mut c_void);

impl Drop for AttributeList {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.0.cast()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_does_not_inherit_secrets() {
        unsafe { std::env::set_var("MEDUSA_TEST_SECRET", "must-not-leak") };
        let directory = tempfile::tempdir().expect("temporary repository");
        let block = environment_block(directory.path()).expect("environment block");
        let decoded = String::from_utf16_lossy(&block);
        assert!(!decoded.contains("MEDUSA_TEST_SECRET"));
        assert!(decoded.contains("MEDUSA_NETWORK=disabled"));
    }

    #[test]
    fn command_line_quotes_arguments() {
        let line = wide_command_line(Path::new("C:\\Program Files\\tool.exe"), &["a b".into()]);
        let decoded = String::from_utf16_lossy(&line);
        assert!(decoded.starts_with("\"C:\\Program Files\\tool.exe\""));
        assert!(decoded.contains("\"a b\""));
    }
}
