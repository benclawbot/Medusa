//! Native Windows command containment.
//!
//! The process is launched in an AppContainer with no network capabilities, attached to a
//! kill-on-close Job Object, and receives only an explicit environment allowlist. Repository
//! access is granted to the AppContainer SID for the lifetime of the command. Every setup step
//! fails closed.

use std::{
    ffi::{OsStr, OsString, c_void},
    io,
    mem::{size_of, zeroed},
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    process::{Command, Output},
    ptr::{null, null_mut},
    slice,
    time::Duration,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE},
    Security::{
        Authorization::{ConvertSidToStringSidW, SECURITY_CAPABILITIES},
        FreeSid, PSID,
    },
    Storage::FileSystem::ReadFile,
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
            JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        },
        Memory::LocalFree,
        Pipes::CreatePipe,
        Threading::{
            CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
            DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, INFINITE,
            InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
            PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW,
            UpdateProcThreadAttribute, WaitForSingleObject,
        },
    },
    UI::Shell::{CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName},
};

const PROFILE_NAME: &str = "Medusa.CommandSandbox";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

pub(crate) fn run(repo: &Path, program: &str, args: &[String]) -> MedusaResult<Output> {
    let root = repo.canonicalize()?;
    let profile = AppContainerProfile::open_or_create()?;
    let sid = profile.sid_string()?;
    let executable = resolve_program(program)?;

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
) -> MedusaResult<Output> {
    let job = OwnedHandle::new(CreateJobObjectW(null(), null()))
        .map_err(|error| unavailable("CreateJobObjectW", error))?;
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
        | JOB_OBJECT_LIMIT_JOB_MEMORY;
    limits.BasicLimitInformation.ActiveProcessLimit = 64;
    limits.JobMemoryLimit = 2 * 1024 * 1024 * 1024;
    if SetInformationJobObject(
        job.0,
        JobObjectExtendedLimitInformation,
        &limits as *const _ as *const c_void,
        size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
    ) == 0
    {
        return Err(unavailable(
            "SetInformationJobObject",
            io::Error::last_os_error(),
        ));
    }

    let mut stdout_read = 0;
    let mut stdout_write = 0;
    let mut stderr_read = 0;
    let mut stderr_write = 0;
    if CreatePipe(&mut stdout_read, &mut stdout_write, null(), 0) == 0
        || CreatePipe(&mut stderr_read, &mut stderr_write, null(), 0) == 0
    {
        return Err(unavailable("CreatePipe", io::Error::last_os_error()));
    }
    let stdout_read = OwnedHandle(stdout_read);
    let stdout_write = OwnedHandle(stdout_write);
    let stderr_read = OwnedHandle(stderr_read);
    let stderr_write = OwnedHandle(stderr_write);

    let mut attribute_bytes = 0usize;
    InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut attribute_bytes);
    if attribute_bytes == 0 {
        return Err(unavailable(
            "InitializeProcThreadAttributeList(size)",
            io::Error::last_os_error(),
        ));
    }
    let mut attributes = vec![0u8; attribute_bytes];
    let attribute_list = attributes.as_mut_ptr().cast();
    if InitializeProcThreadAttributeList(attribute_list, 1, 0, &mut attribute_bytes) == 0 {
        return Err(unavailable(
            "InitializeProcThreadAttributeList",
            io::Error::last_os_error(),
        ));
    }
    let _attributes = AttributeList(attribute_list);

    let security = SECURITY_CAPABILITIES {
        AppContainerSid: sid,
        Capabilities: null_mut(),
        CapabilityCount: 0,
        Reserved: 0,
    };
    if UpdateProcThreadAttribute(
        attribute_list,
        0,
        PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
        &security as *const _ as *mut c_void,
        size_of::<SECURITY_CAPABILITIES>(),
        null_mut(),
        null_mut(),
    ) == 0
    {
        return Err(unavailable(
            "UpdateProcThreadAttribute(AppContainer)",
            io::Error::last_os_error(),
        ));
    }

    let mut startup: STARTUPINFOEXW = zeroed();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdOutput = stdout_write.0;
    startup.StartupInfo.hStdError = stderr_write.0;
    startup.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
    startup.lpAttributeList = attribute_list;

    let mut process: PROCESS_INFORMATION = zeroed();
    let mut command_line = wide_command_line(executable, args);
    let mut environment = environment_block(root);
    let root_wide = wide_null(root.as_os_str());
    let executable_wide = wide_null(executable.as_os_str());

    if CreateProcessW(
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
    ) == 0
    {
        return Err(unavailable(
            "CreateProcessW(AppContainer)",
            io::Error::last_os_error(),
        ));
    }
    let process_handle = OwnedHandle(process.hProcess);
    let thread_handle = OwnedHandle(process.hThread);

    if AssignProcessToJobObject(job.0, process_handle.0) == 0 {
        return Err(unavailable(
            "AssignProcessToJobObject",
            io::Error::last_os_error(),
        ));
    }
    if ResumeThread(thread_handle.0) == u32::MAX {
        return Err(unavailable("ResumeThread", io::Error::last_os_error()));
    }
    drop(stdout_write);
    drop(stderr_write);

    let wait = WaitForSingleObject(process_handle.0, COMMAND_TIMEOUT.as_millis() as u32);
    if wait != 0 {
        return Err(MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            "Windows AppContainer command timed out or wait failed",
        ));
    }

    let stdout = read_all(stdout_read.0)?;
    let stderr = read_all(stderr_read.0)?;
    let status = std::process::ExitStatus::from_raw(process.dwProcessId as i32);
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn resolve_program(program: &str) -> MedusaResult<PathBuf> {
    let output = Command::new("where.exe")
        .arg(program)
        .output()
        .map_err(|error| unavailable("where.exe", error))?;
    if !output.status.success() {
        return Err(unavailable(
            "program resolution",
            io::Error::new(io::ErrorKind::NotFound, program.to_owned()),
        ));
    }
    let first = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .ok_or_else(|| {
            unavailable(
                "program resolution",
                io::Error::from(io::ErrorKind::NotFound),
            )
        })?;
    PathBuf::from(first).canonicalize().map_err(Into::into)
}

fn environment_block(root: &Path) -> Vec<u16> {
    let temp = root.join(".medusa-sandbox-tmp");
    let _ = std::fs::create_dir_all(&temp);
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
    block
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
    format!("\"{}\"", value.replace('"', "\\\""))
}

fn wide_null(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain([0]).collect()
}

unsafe fn read_all(handle: HANDLE) -> MedusaResult<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let mut read = 0u32;
        if ReadFile(
            handle,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            &mut read,
            null_mut(),
        ) == 0
        {
            let code = GetLastError();
            if code == 109 {
                break;
            }
            return Err(unavailable(
                "ReadFile",
                io::Error::from_raw_os_error(code as i32),
            ));
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
    fn open_or_create() -> MedusaResult<Self> {
        unsafe {
            let name = wide_null(OsStr::new(PROFILE_NAME));
            let mut sid = null_mut();
            let mut hr = DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid);
            if hr < 0 {
                let display = wide_null(OsStr::new("Medusa command sandbox"));
                let description = wide_null(OsStr::new("Network-isolated Medusa command runner"));
                hr = CreateAppContainerProfile(
                    name.as_ptr(),
                    display.as_ptr(),
                    description.as_ptr(),
                    null(),
                    0,
                    &mut sid,
                );
            }
            if hr < 0 || sid.is_null() {
                return Err(unavailable(
                    "AppContainer profile",
                    io::Error::from_raw_os_error(hr),
                ));
            }
            Ok(Self { sid })
        }
    }

    fn sid_string(&self) -> MedusaResult<String> {
        unsafe {
            let mut text = null_mut();
            if ConvertSidToStringSidW(self.sid, &mut text) == 0 {
                return Err(unavailable(
                    "ConvertSidToStringSidW",
                    io::Error::last_os_error(),
                ));
            }
            let len = (0..).take_while(|&i| *text.add(i) != 0).count();
            let value = OsString::from_wide(slice::from_raw_parts(text, len))
                .to_string_lossy()
                .into_owned();
            LocalFree(text as isize);
            Ok(value)
        }
    }
}

impl Drop for AppContainerProfile {
    fn drop(&mut self) {
        unsafe {
            FreeSid(self.sid);
        }
    }
}

struct AclGrant {
    path: PathBuf,
    sid: String,
}

impl AclGrant {
    fn grant(path: &Path, sid: &str, rights: &str) -> MedusaResult<Self> {
        let status = Command::new("icacls.exe")
            .arg(path)
            .args(["/grant", &format!("*{sid}:{rights}"), "/Q"])
            .status()
            .map_err(|error| unavailable("icacls grant", error))?;
        if !status.success() {
            return Err(unavailable(
                "icacls grant",
                io::Error::other(format!("exit status {status}")),
            ));
        }
        Ok(Self {
            path: path.to_owned(),
            sid: sid.to_owned(),
        })
    }
}

impl Drop for AclGrant {
    fn drop(&mut self) {
        let _ = Command::new("icacls.exe")
            .arg(&self.path)
            .args(["/remove", &format!("*{}", self.sid), "/Q"])
            .status();
    }
}

struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn new(handle: HANDLE) -> io::Result<Self> {
        if handle == 0 || handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(handle))
        }
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct AttributeList(*mut c_void);
impl Drop for AttributeList {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.0.cast()) }
    }
}

fn unavailable(operation: &str, error: io::Error) -> MedusaError {
    let mut result = MedusaError::new(
        ErrorCode::SandboxUnavailable,
        ErrorCategory::Environment,
        format!("Windows AppContainer sandbox unavailable during {operation}: {error}"),
    );
    result.context.insert(
        "sandbox_backend".into(),
        serde_json::Value::String("windows_appcontainer".into()),
    );
    result.context.insert(
        "effective_restrictions".into(),
        serde_json::json!([
            "app_container",
            "network_denied",
            "job_kill_on_close",
            "environment_allowlist"
        ]),
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_does_not_inherit_secrets() {
        std::env::set_var("MEDUSA_TEST_SECRET", "must-not-leak");
        let block = environment_block(Path::new("C:\\workspace"));
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
