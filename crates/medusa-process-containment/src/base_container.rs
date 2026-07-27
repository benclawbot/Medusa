use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString, c_void},
    fs, io,
    mem::{size_of, transmute, zeroed},
    os::windows::{ffi::OsStrExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{ExitStatus, Output},
    ptr::{null, null_mut},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use flatbuffers::FlatBufferBuilder;
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, FARPROC, FreeLibrary, HANDLE, HMODULE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
        WAIT_TIMEOUT,
    },
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
            JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        },
        LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW},
        Threading::{
            CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, GetExitCodeProcess,
            PROCESS_INFORMATION, ResumeThread, STARTUPINFOW, WaitForSingleObject,
        },
    },
};

const SANDBOX_IDENTITY: &str = "Medusa.CommandSandbox.v2";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const PROCESSMODEL_DLL: &str = "processmodel.dll";
const SANDBOX_EXPORT: &[u8] = b"Experimental_CreateProcessInSandbox\0";

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

/// Runs a command in the Windows composable sandbox.
///
/// The operating system supplies AppContainer network isolation and Bound File
/// System grants. The repository is the only read/write host path; toolchain and
/// system locations are read-only. The function fails closed when the Windows
/// 11 experimental sandbox API is unavailable.
pub fn run_appcontainer(repo: &Path, program: &str, args: &[String]) -> io::Result<Output> {
    let root = strip_verbatim(&repo.canonicalize()?);
    let executable = strip_verbatim(&resolve_program(program)?);
    let temp = SandboxTemp::new(&root)?;
    temp.write_script(&executable, args)?;

    let read_only = read_only_paths(&executable);
    let specification = sandbox_specification(&root, &read_only);
    let api = SandboxApi::load()?;
    let result = unsafe { launch(&api, &root, &temp, &specification) };
    result.and_then(|status| temp.output(status))
}

unsafe fn launch(
    api: &SandboxApi,
    root: &Path,
    temp: &SandboxTemp,
    specification: &[u8],
) -> io::Result<ExitStatus> {
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

    let cmd = system_command("cmd.exe")?;
    let mut command_line = wide_command_line(
        &cmd,
        &[
            "/D".into(),
            "/S".into(),
            "/C".into(),
            temp.script.display().to_string(),
        ],
    );
    let mut environment = environment_block(root)?;
    let cmd_wide = wide_null(cmd.as_os_str());
    let root_wide = wide_null(root.as_os_str());
    let identity = wide_null(OsStr::new(SANDBOX_IDENTITY));
    let mut startup: STARTUPINFOW = unsafe { zeroed() };
    startup.cb = size_of::<STARTUPINFOW>() as u32;
    let mut process: PROCESS_INFORMATION = unsafe { zeroed() };

    let created = unsafe {
        (api.create)(
            cmd_wide.as_ptr(),
            command_line.as_mut_ptr(),
            null(),
            null(),
            0,
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
        let error = io::Error::last_os_error();
        return Err(io::Error::new(
            error.kind(),
            format!("Windows composable sandbox process creation failed: {error}"),
        ));
    }

    let process_handle = OwnedHandle::new(process.hProcess)?;
    let thread_handle = OwnedHandle::new(process.hThread)?;
    if unsafe { AssignProcessToJobObject(job.0, process_handle.0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { ResumeThread(thread_handle.0) } == u32::MAX {
        return Err(io::Error::last_os_error());
    }

    let wait = unsafe { WaitForSingleObject(process_handle.0, COMMAND_TIMEOUT.as_millis() as u32) };
    if wait == WAIT_TIMEOUT {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "Windows composable sandbox command timed out after {} seconds",
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
    Ok(ExitStatus::from_raw(exit_code))
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

struct SandboxTemp {
    directory: PathBuf,
    script: PathBuf,
    stdout: PathBuf,
    stderr: PathBuf,
}

impl SandboxTemp {
    fn new(root: &Path) -> io::Result<Self> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory = root
            .join(".medusa-sandbox-tmp")
            .join(format!("{}-{nonce}", std::process::id()));
        fs::create_dir_all(&directory)?;
        Ok(Self {
            script: directory.join("command.cmd"),
            stdout: directory.join("stdout.bin"),
            stderr: directory.join("stderr.bin"),
            directory,
        })
    }

    fn write_script(&self, executable: &Path, args: &[String]) -> io::Result<()> {
        let mut command = quote(executable.as_os_str());
        for argument in args {
            command.push(' ');
            command.push_str(&quote(OsStr::new(argument)));
        }
        let script = format!(
            "@echo off\r\n{command} 1>\"{}\" 2>\"{}\"\r\nexit /b %errorlevel%\r\n",
            self.stdout.display(),
            self.stderr.display()
        );
        fs::write(&self.script, script)
    }

    fn output(&self, status: ExitStatus) -> io::Result<Output> {
        Ok(Output {
            status,
            stdout: fs::read(&self.stdout).unwrap_or_default(),
            stderr: fs::read(&self.stderr).unwrap_or_default(),
        })
    }
}

impl Drop for SandboxTemp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
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

fn system_command(name: &str) -> io::Result<PathBuf> {
    let root = std::env::var_os("SystemRoot")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "SystemRoot is not set"))?;
    PathBuf::from(root)
        .join("System32")
        .join(name)
        .canonicalize()
}

fn environment_block(root: &Path) -> io::Result<Vec<u16>> {
    let temp = root.join(".medusa-sandbox-tmp");
    fs::create_dir_all(&temp)?;
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
    fn command_line_quotes_arguments() {
        let line = wide_command_line(Path::new("C:\\Program Files\\tool.exe"), &["a b".into()]);
        let decoded = String::from_utf16_lossy(&line);
        assert!(decoded.starts_with("\"C:\\Program Files\\tool.exe\""));
        assert!(decoded.contains("\"a b\""));
    }
}
