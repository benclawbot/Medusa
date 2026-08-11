from pathlib import Path

base = Path('crates/medusa-process-containment/src/base_container.rs')
s = base.read_text()
if 'use crate::ProcessOwnershipReceipt;' not in s:
    s = s.replace('use flatbuffers::FlatBufferBuilder;\n', 'use flatbuffers::FlatBufferBuilder;\nuse crate::ProcessOwnershipReceipt;\n', 1)

if 'pub struct WindowsSandboxLimits' not in s:
    anchor = '\n/// Runs a command directly in the Windows composable sandbox.\n'
    limits = '''
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
'''
    if anchor not in s:
        raise SystemExit('Windows sandbox API anchor missing')
    s = s.replace(anchor, '\n' + limits + anchor, 1)

old = '''pub fn run_appcontainer_cancellable(
    repo: &Path,
    program: &str,
    args: &[String],
    cancellation: &AtomicBool,
) -> io::Result<Output> {
    let root = strip_verbatim(&repo.canonicalize()?);
    let executable = strip_verbatim(&resolve_program(program)?);
    let read_only = read_only_paths(&executable);
    let specification = sandbox_specification(&root, &read_only);
    let api = SandboxApi::load()?;
    unsafe { launch(&api, &root, &executable, args, &specification, cancellation) }
}
'''
new = '''pub fn run_appcontainer_cancellable(
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
        launch(
            &api,
            &root,
            &executable,
            args,
            &specification,
            cancellation,
            limits,
            &mut on_start,
        )
    }
}
'''
if 'run_appcontainer_cancellable_observed' not in s:
    if old not in s:
        raise SystemExit('cancellable function marker missing')
    s = s.replace(old, new, 1)

sig_old = '''    specification: &[u8],
    cancellation: &AtomicBool,
) -> io::Result<Output> {
'''
sig_new = '''    specification: &[u8],
    cancellation: &AtomicBool,
    sandbox_limits: WindowsSandboxLimits,
    on_start: &mut impl FnMut(&ProcessOwnershipReceipt) -> io::Result<()>,
) -> io::Result<Output> {
'''
if 'sandbox_limits: WindowsSandboxLimits' not in s:
    if sig_old not in s:
        raise SystemExit('launch signature marker missing')
    s = s.replace(sig_old, sig_new, 1)

s = s.replace('JOB_OBJECT_LIMIT_JOB_MEMORY,\n', 'JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_PROCESS_TIME,\n', 1)
s = s.replace('limits.BasicLimitInformation.ActiveProcessLimit = 64;', 'limits.BasicLimitInformation.ActiveProcessLimit = sandbox_limits.active_process_limit;', 1)
s = s.replace('limits.JobMemoryLimit = 2 * 1024 * 1024 * 1024;', 'limits.JobMemoryLimit = sandbox_limits.job_memory_bytes;', 1)
flag_anchor = '''    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
        | JOB_OBJECT_LIMIT_JOB_MEMORY;
'''
if 'PerProcessUserTimeLimit = sandbox_limits.process_user_time_100ns' not in s:
    repl = flag_anchor + '''    if sandbox_limits.process_user_time_100ns > 0 {
        limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_TIME;
        limits.BasicLimitInformation.PerProcessUserTimeLimit = sandbox_limits.process_user_time_100ns;
    }
'''
    if flag_anchor not in s:
        raise SystemExit('job limit flags marker missing')
    s = s.replace(flag_anchor, repl, 1)

handle_anchor = '''    let process_handle = OwnedHandle::new(process.hProcess)?;
    let thread_handle = OwnedHandle::new(process.hThread)?;
'''
if 'failed to capture Windows sandbox process identity' not in s:
    repl = handle_anchor + '''    let ownership = ProcessOwnershipReceipt::capture(process.dwProcessId).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("failed to capture Windows sandbox process identity: {error}"),
        )
    })?;
    on_start(&ownership)?;
'''
    if handle_anchor not in s:
        raise SystemExit('process handle marker missing')
    s = s.replace(handle_anchor, repl, 1)

s = s.replace('if started.elapsed() >= COMMAND_TIMEOUT {', 'if started.elapsed() >= sandbox_limits.timeout {', 1)
s = s.replace('COMMAND_TIMEOUT.as_secs()', 'sandbox_limits.timeout.as_secs()', 1)
base.write_text(s)

lib = Path('crates/medusa-process-containment/src/lib.rs')
s = lib.read_text()
s = s.replace(
    '    WindowsSandboxRestrictions, run_appcontainer, run_appcontainer_cancellable,\n',
    '    WindowsSandboxLimits, WindowsSandboxRestrictions, run_appcontainer,\n    run_appcontainer_cancellable, run_appcontainer_cancellable_observed,\n',
)
lib.write_text(s)

policy = Path('crates/medusa-agent/src/policy.rs')
s = policy.read_text()
s = s.replace(
    '#[cfg(any(target_os = "linux", target_os = "macos"))]\n#[path = "analysis_process_tracker.rs"]\nmod analysis_process_tracker;\n',
    '#[cfg(any(target_os = "linux", target_os = "macos", windows))]\n#[path = "analysis_process_tracker.rs"]\nmod analysis_process_tracker;\n',
)
policy.write_text(s)

win = Path('crates/medusa-agent/src/windows_sandbox.rs')
s = win.read_text()
s = s.replace(
    'use medusa_process_containment::{WindowsSandboxRestrictions, run_appcontainer_cancellable};',
    'use medusa_process_containment::{\n    WindowsSandboxLimits, WindowsSandboxRestrictions, run_appcontainer_cancellable_observed,\n};\n\nuse super::analysis_process_tracker::AnalysisProcessTracker;',
)
old_body = '''    run_appcontainer_cancellable(repo, program, args, cancellation).map_err(|error| {
        if error.kind() == std::io::ErrorKind::Interrupted {
            cancelled(error)
        } else {
            unavailable(error)
        }
    })
'''
new_body = '''    let analysis = repo
        .components()
        .any(|component| component.as_os_str() == "analysis-workspace-v1");
    let limits = if analysis {
        WindowsSandboxLimits::analysis()
    } else {
        WindowsSandboxLimits::default()
    };
    let mut tracker = None;
    let result = run_appcontainer_cancellable_observed(
        repo,
        program,
        args,
        cancellation,
        limits,
        |receipt| {
            if analysis {
                tracker = Some(
                    AnalysisProcessTracker::started(repo, program, args, receipt)
                        .map_err(|error| std::io::Error::other(error.to_string()))?,
                );
            }
            Ok(())
        },
    );
    match result {
        Ok(output) => {
            if let Some(tracker) = tracker.take() {
                tracker.exited(output.status.code())?;
            }
            Ok(output)
        }
        Err(error) => {
            if let Some(tracker) = tracker.take() {
                let _ = tracker.failed(&error.to_string());
            }
            Err(if error.kind() == std::io::ErrorKind::Interrupted {
                cancelled(error)
            } else {
                unavailable(error)
            })
        }
    }
'''
if 'WindowsSandboxLimits::analysis()' not in s:
    if old_body not in s:
        raise SystemExit('Windows adapter body marker missing')
    s = s.replace(old_body, new_body, 1)
win.write_text(s)

contained = Path('crates/medusa-runtime/src/analysis_contained.rs')
s = contained.read_text()
s = s.replace('''    #[cfg(windows)]
    {
        64
    }''', '''    #[cfg(windows)]
    {
        1
    }''', 1)
s = s.replace('''    #[cfg(windows)]
    {
        2 * 1024 * 1024 * 1024
    }''', '''    #[cfg(windows)]
    {
        UNIX_ANALYSIS_MEMORY_BYTES
    }''', 1)
s = s.replace('''    #[cfg(windows)]
    {
        CONTAINMENT_WALL_SECONDS
    }''', '''    #[cfg(windows)]
    {
        UNIX_ANALYSIS_CPU_SECONDS
    }''', 1)
contained.write_text(s)
