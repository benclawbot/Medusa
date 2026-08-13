use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString, c_void},
    io,
    mem::{size_of, transmute, zeroed},
    os::windows::{ffi::OsStrExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{ExitStatus, Output},
    ptr::{null, null_mut},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use crate::ProcessOwnershipReceipt;
use flatbuffers::FlatBufferBuilder;
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, FARPROC, FreeLibrary, HANDLE, HANDLE_FLAG_INHERIT, HMODULE,
        INVALID_HANDLE_VALUE, SetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Security::SECURITY_ATTRIBUTES,
    Storage::FileSystem::ReadFile,
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
            JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        },
        LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW},
        Pipes::CreatePipe,
        Threading::{
            CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, GetExitCodeProcess,
            PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOW,
            WaitForSingleObject,
        },
    },
};

const SANDBOX_IDENTITY: &str = "Medusa.CommandSandbox.v2";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const PROCESSMODEL_DLL: &str = "processmodel.dll";
const SANDBOX_EXPORT: &[u8] = b"Experimental_CreateProcessInSandbox\0";
const BROKEN_PIPE: u32 = 109;
const ERROR_CALL_NOT_IMPLEMENTED: i32 = 120;
// Win32 JOB_OBJECT_LIMIT_PROCESS_TIME (winnt.h). Kept local because this windows-sys feature surface does not export it.
const JOB_OBJECT_LIMIT_PROCESS_TIME_FLAG: u32 = 0x0000_0002;
// Reserved by Experimental_CreateProcessInSandbox and required to be FALSE.
// TRUE fails with ERROR_NOT_SUPPORTED before process creation.
const INHERIT_HANDLES: i32 = 0;

type CreateProcessInSandbox = unsafe extern "system" fn(
    *const u16,
    *mut u16,
    *const c_void,
    *const c_void,
    i32,
    u32,
    *mut c_void,
    *const u16,
    *mut STARTUPINFOW,
    *const u16,
    *const c_void,
    u32,
    *mut PROCESS_INFORMATION,
) -> i32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsSandboxRestrictions {
    pub backend: &'static str,
    pub restrictions: &'static [&'static str],
}

impl Default for WindowsSandboxRestrictions {
    fn default() -> Self {
        Self {
            backend: "windows_base_container",
            restrictions: &[
                "app_container",
                "network_denied",
                "bound_filesystem_repository_rw",
                "bound_filesystem_toolchain_ro",
                "job_kill_on_close",
                "active_process_limit",
                "job_memory_limit",
                "environment_allowlist",
                "no_host_acl_mutation",
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowsSandboxLimits {
    pub active_process_limit: u32,
    pub job_memory_bytes: usize,
    pub process_user_time_100ns: i64,
    pub timeout: Duration,
}

impl Default for WindowsSandboxLimits {
    fn default() -> Self {
        Self {
            active_process_limit: 64,
            job_memory_bytes: 2 * 1024 * 1024 * 1024,
            process_user_time_100ns: 0,
            timeout: COMMAND_TIMEOUT,
        }
    }
}

impl WindowsSandboxLimits {
    #[must_use]
    pub const fn analysis() -> Self {
        Self {
            active_process_limit: 1,
            job_memory_bytes: 512 * 1024 * 1024,
            process_user_time_100ns: 10 * 10_000_000,
            timeout: COMMAND_TIMEOUT,
        }
    }
}

/// Runs a command directly in the Windows composable sandbox.
///
/// No shell or batch file is involved, so arguments cannot be reinterpreted as
/// shell syntax after they cross the command-policy boundary.
pub fn run_appcontainer(repo: &Path, program: &str, args: &[String]) -> io::Result<Output> {
    let cancellation = AtomicBool::new(false);
    run_appcontainer_cancellable(repo, program, args, &cancellation)
}

pub fn run_appcontainer_cancellable(
    repo: &Path,
    program: &str,
    args: &[String],
    cancellation: &AtomicBool,
) -> io::Result<Output> {
    run_appcontainer_cancellable_observed(
        repo,
        program,
        args,
        cancellation,
        WindowsSandboxLimits::default(),
        |_| Ok(()),
    )
}

pub fn run_appcontainer_cancellable_observed<F>(
    repo: &Path,
    program: &str,
    args: &[String],
    cancellation: &AtomicBool,
    limits: WindowsSandboxLimits,
    mut on_start: F,
) -> io::Result<Output>
where
    F: FnMut(&ProcessOwnershipReceipt) -> io::Result<()>,
{
    let root = strip_verbatim(&repo.canonicalize()?);
    let executable = strip_verbatim(&resolve_program(program)?);
    let read_only = read_only_paths(&executable);
    let specification = sandbox_specification(&root, &read_only);
    let api = SandboxApi::load()?;
    unsafe {
        let mut controls = LaunchControls {
            limits,
            on_start: &mut on_start,
        };
        launch(
            &api,
            &root,
            &executable,
            args,
            &specification,
            cancellation,
            &mut controls,
        )
    }
}

struct LaunchControls<'a> {
    limits: WindowsSandboxLimits,
    on_start: &'a mut dyn FnMut(&ProcessOwnershipReceipt) -> io::Result<()>,
}

unsafe fn launch(
    api: &SandboxApi,
    root: &Path,
    executable: &Path,
    args: &[String],
    specification: &[u8],
    cancellation: &AtomicBool,
    controls: &mut LaunchControls<'_>,
) -> io::Result<Output> {
    let sandbox_limits = controls.limits;
    let job = OwnedHandle::new(unsafe { CreateJobObjectW(null(), null()) })?;
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
        | JOB_OBJECT_LIMIT_JOB_MEMORY;
    if sandbox_limits.process_user_time_100ns > 0 {
        limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_TIME_FLAG;
        limits.BasicLimitInformation.PerProcessUserTimeLimit =
            sandbox_limits.process_user_time_100ns;
    }
    limits.BasicLimitInformation.ActiveProcessLimit = sandbox_limits.active_process_limit;
    limits.JobMemoryLimit = sandbox_limits.job_memory_bytes;
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
    let mut command_line = wide_command_line(executable, args);
    let mut environment = environment_block(root)?;
    let executable_wide = wide_null(executable.as_os_str());
    let root_wide = wide_null(root.as_os_str());
    let identity = wide_null(OsStr::new(SANDBOX_IDENTITY));
    let mut startup: STARTUPINFOW = unsafe { zeroed() };
    startup.cb = size_of::<STARTUPINFOW>() as u32;
    startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdInput = INVALID_HANDLE_VALUE;
    startup.hStdOutput = stdout_write.0;
    startup.hStdError = stderr_write.0;
    let mut process: PROCESS_INFORMATION = unsafe { zeroed() };

    let created = unsafe {
        (api.create)(
            executable_wide.as_ptr(),
            command_line.as_mut_ptr(),
            null(),
            null(),
            INHERIT_HANDLES,
            CREATE_SUSPENDED | CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            environment.as_mut_ptr().cast(),
            root_wide.as_ptr(),
            &mut startup,
            identity.as_ptr(),
            specification.as_ptr().cast(),
            specification.len() as u32,
            &mut process,
        )
    };
    if created == 0 {
        return Err(sandbox_process_creation_error(io::Error::last_os_error()));
    }

    let process_handle = OwnedHandle::new(process.hProcess)?;
    let thread_handle = OwnedHandle::new(process.hThread)?;
    let ownership = ProcessOwnershipReceipt::capture(process.dwProcessId).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("failed to capture Windows sandbox process identity: {error}"),
        )
    })?;
    if unsafe { AssignProcessToJobObject(job.0, process_handle.0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    (controls.on_start)(&ownership)?;
    if unsafe { ResumeThread(thread_handle.0) } == u32::MAX {
        return Err(io::Error::last_os_error());
    }
    drop(stdout_write);
    drop(stderr_write);

    let stdout_reader = thread::spawn(move || read_all(stdout_read));
    let stderr_reader = thread::spawn(move || read_all(stderr_read));
    let started = Instant::now();
    loop {
        if cancellation.load(Ordering::Acquire) {
            drop(job);
            let _ = join_reader(stdout_reader);
            let _ = join_reader(stderr_reader);
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Windows composable sandbox command cancelled",
            ));
        }
        let wait = unsafe { WaitForSingleObject(process_handle.0, 50) };
        if wait == WAIT_OBJECT_0 {
            break;
        }
        if wait != WAIT_TIMEOUT {
            drop(job);
            let _ = join_reader(stdout_reader);
            let _ = join_reader(stderr_reader);
            return Err(io::Error::last_os_error());
        }
        if started.elapsed() >= sandbox_limits.timeout {
            drop(job);
            let _ = join_reader(stdout_reader);
            let _ = join_reader(stderr_reader);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "Windows composable sandbox command timed out after {} seconds",
                    sandbox_limits.timeout.as_secs()
                ),
            ));
        }
    }
    let mut exit_code = 0u32;
    if unsafe { GetExitCodeProcess(process_handle.0, &mut exit_code) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let stdout = join_reader(stdout_reader)?;
    let stderr = join_reader(stderr_reader)?;
    Ok(Output {
        status: ExitStatus::from_raw(exit_code),
        stdout,
        stderr,
    })
}

fn sandbox_process_creation_error(error: io::Error) -> io::Error {
    if error.raw_os_error() == Some(ERROR_CALL_NOT_IMPLEMENTED) {
        return io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows composable sandbox API is unavailable; Windows 11 support is required",
        );
    }
    io::Error::new(
        error.kind(),
        format!("Windows composable sandbox process creation failed: {error}"),
    )
}

fn create_inheritable_pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    let mut read: HANDLE = null_mut();
    let mut write: HANDLE = null_mut();
    if unsafe { CreatePipe(&mut read, &mut write, &attributes, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let read = OwnedHandle::new(read)?;
    let write = OwnedHandle::new(write)?;
    if unsafe { SetHandleInformation(read.0, HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((read, write))
}

fn read_all(handle: OwnedHandle) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let mut read = 0u32;
        if unsafe {
            ReadFile(
                handle.0,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut read,
                null_mut(),
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(BROKEN_PIPE as i32) {
                break;
            }
            return Err(error);
        }
        if read == 0 {
            break;
        }
        output.extend_from_slice(&buffer[..read as usize]);
    }
    Ok(output)
}

fn join_reader(reader: thread::JoinHandle<io::Result<Vec<u8>>>) -> io::Result<Vec<u8>> {
    reader
        .join()
        .map_err(|_| io::Error::other("Windows sandbox output reader panicked"))?
}

fn sandbox_specification(root: &Path, read_only: &[PathBuf]) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::new();
    let version = builder.create_string("0.1.0");
    let root_string = builder.create_string(&root.to_string_lossy());
    let read_write = builder.create_vector(&[root_string]);
    let read_only_strings = read_only
        .iter()
        .map(|path| builder.create_string(&path.to_string_lossy()))
        .collect::<Vec<_>>();
    let read_only = builder.create_vector(&read_only_strings);

    let table = builder.start_table();
    builder.push_slot_always(4, version);
    builder.push_slot(6, true, false);
    builder.push_slot(10, true, false);
    builder.push_slot(14, true, false);
    builder.push_slot_always(18, read_write);
    builder.push_slot_always(20, read_only);
    let specification = builder.end_table(table);
    builder.finish(specification, Some("SBOX"));
    builder.finished_data().to_vec()
}

fn read_only_paths(executable: &Path) -> Vec<PathBuf> {
    let mut paths = BTreeSet::new();
    if let Some(parent) = executable.parent() {
        paths.insert(parent.to_path_buf());
    }
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path).filter(|path| path.is_dir()));
    }
    for variable in [
        "SystemRoot",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramData",
        "CARGO_HOME",
        "RUSTUP_HOME",
    ] {
        if let Some(path) = std::env::var_os(variable).map(PathBuf::from)
            && path.is_dir()
        {
            paths.insert(path);
        }
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
        for relative in [".cargo", ".rustup"] {
            let path = profile.join(relative);
            if path.is_dir() {
                paths.insert(path);
            }
        }
    }
    paths
        .into_iter()
        .map(|path| strip_verbatim(&path))
        .collect()
}

struct SandboxApi {
    module: HMODULE,
    create: CreateProcessInSandbox,
}

impl SandboxApi {
    fn load() -> io::Result<Self> {
        let dll = wide_null(OsStr::new(PROCESSMODEL_DLL));
        let module =
            unsafe { LoadLibraryExW(dll.as_ptr(), null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) };
        if module.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Windows composable sandbox API is unavailable; Windows 11 support is required",
            ));
        }
        let raw: FARPROC = unsafe { GetProcAddress(module, SANDBOX_EXPORT.as_ptr()) };
        let Some(raw) = raw else {
            unsafe { FreeLibrary(module) };
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Experimental_CreateProcessInSandbox is unavailable; refusing an unsandboxed fallback",
            ));
        };
        let create = unsafe {
            transmute::<unsafe extern "system" fn() -> isize, CreateProcessInSandbox>(raw)
        };
        Ok(Self { module, create })
    }
}

impl Drop for SandboxApi {
    fn drop(&mut self) {
        unsafe { FreeLibrary(self.module) };
    }
}

fn resolve_program(program: &str) -> io::Result<PathBuf> {
    let requested = Path::new(program);
    if requested.is_absolute() || requested.components().count() > 1 {
        return requested.canonicalize();
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let extensions =
        std::env::var_os("PATHEXT").unwrap_or_else(|| OsString::from(".COM;.EXE;.BAT;.CMD"));
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
        ("MEDUSA_SANDBOX", OsString::from("windows-base-container")),
        ("MEDUSA_NETWORK", OsString::from("disabled")),
    ];
    for key in ["CARGO_HOME", "RUSTUP_HOME"] {
        if let Some(value) = std::env::var_os(key) {
            values.push((key, value));
        }
    }
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

fn strip_verbatim(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => path.to_path_buf(),
    }
}

struct OwnedHandle(HANDLE);

unsafe impl Send for OwnedHandle {}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specification_has_sbox_identifier() {
        let bytes = sandbox_specification(Path::new(r"C:\repo"), &[PathBuf::from(r"C:\tools")]);
        assert_eq!(&bytes[4..8], b"SBOX");
    }

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
    fn command_line_preserves_shell_metacharacters_as_arguments() {
        let line = wide_command_line(
            Path::new("C:\\Program Files\\tool.exe"),
            &["foo|bar".into(), "a b".into()],
        );
        let decoded = String::from_utf16_lossy(&line);
        assert!(decoded.starts_with("\"C:\\Program Files\\tool.exe\""));
        assert!(decoded.contains("foo|bar"));
        assert!(decoded.contains("\"a b\""));
    }

    #[test]
    fn sandbox_process_does_not_request_reserved_handle_inheritance() {
        assert_eq!(INHERIT_HANDLES, 0);
    }

    #[test]
    fn unsupported_process_creation_has_a_locale_independent_error() {
        let error = sandbox_process_creation_error(io::Error::from_raw_os_error(
            ERROR_CALL_NOT_IMPLEMENTED,
        ));
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert_eq!(
            error.to_string(),
            "Windows composable sandbox API is unavailable; Windows 11 support is required"
        );
    }
}
