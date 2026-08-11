from pathlib import Path

base = Path('crates/medusa-process-containment/src/base_container.rs')
s = base.read_text()
s = s.replace('JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_PROCESS_TIME,', 'JOB_OBJECT_LIMIT_JOB_MEMORY,')
if 'JOB_OBJECT_LIMIT_PROCESS_TIME_FLAG' not in s:
    anchor = 'const BROKEN_PIPE: u32 = 109;\n'
    if anchor not in s:
        raise SystemExit('BROKEN_PIPE anchor missing')
    s = s.replace(
        anchor,
        anchor + '// Win32 JOB_OBJECT_LIMIT_PROCESS_TIME (winnt.h). Kept local because this windows-sys feature surface does not export it.\nconst JOB_OBJECT_LIMIT_PROCESS_TIME_FLAG: u32 = 0x0000_0002;\n',
        1,
    )
s = s.replace(
    'limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_TIME;',
    'limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_TIME_FLAG;',
)

if 'struct LaunchControls' not in s:
    anchor = '\nunsafe fn launch(\n'
    controls = '''
struct LaunchControls<'a> {
    limits: WindowsSandboxLimits,
    on_start: &'a mut dyn FnMut(&ProcessOwnershipReceipt) -> io::Result<()>,
}
'''
    if anchor not in s:
        raise SystemExit('launch anchor missing')
    s = s.replace(anchor, '\n' + controls + anchor, 1)

old_call = '''        launch(
            &api,
            &root,
            &executable,
            args,
            &specification,
            cancellation,
            limits,
            &mut on_start,
        )
'''
new_call = '''        let mut controls = LaunchControls {
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
'''
if old_call in s:
    s = s.replace(old_call, new_call, 1)

old_sig = '''    specification: &[u8],
    cancellation: &AtomicBool,
    sandbox_limits: WindowsSandboxLimits,
    on_start: &mut impl FnMut(&ProcessOwnershipReceipt) -> io::Result<()>,
) -> io::Result<Output> {
'''
new_sig = '''    specification: &[u8],
    cancellation: &AtomicBool,
    controls: &mut LaunchControls<'_>,
) -> io::Result<Output> {
    let sandbox_limits = controls.limits;
'''
if old_sig in s:
    s = s.replace(old_sig, new_sig, 1)
s = s.replace('    on_start(&ownership)?;', '    (controls.on_start)(&ownership)?;', 1)

# The process is created suspended. Establish Job Object ownership before the
# durable start callback so a persistence failure closes the Job and kills the
# child; still invoke the callback before ResumeThread so no user code runs
# without a durable native-identity record.
pre_assign = '''    let ownership = ProcessOwnershipReceipt::capture(process.dwProcessId).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("failed to capture Windows sandbox process identity: {error}"),
        )
    })?;
    (controls.on_start)(&ownership)?;
    if unsafe { AssignProcessToJobObject(job.0, process_handle.0) } == 0 {
        return Err(io::Error::last_os_error());
    }
'''
owned_then_recorded = '''    let ownership = ProcessOwnershipReceipt::capture(process.dwProcessId).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("failed to capture Windows sandbox process identity: {error}"),
        )
    })?;
    if unsafe { AssignProcessToJobObject(job.0, process_handle.0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    (controls.on_start)(&ownership)?;
'''
if pre_assign in s:
    s = s.replace(pre_assign, owned_then_recorded, 1)
base.write_text(s)

tree = Path('crates/medusa-process-containment/src/process_tree.rs')
s = tree.read_text()
s = s.replace(
    '#[cfg(any(unix, windows))]\nuse crate::{ProcessOwnershipReceipt, ProcessOwnershipVerification};',
    '#[cfg(any(unix, windows))]\nuse crate::ProcessOwnershipReceipt;\n#[cfg(unix)]\nuse crate::ProcessOwnershipVerification;',
)
s = s.replace(
    '    use super::*;\n\n    const ROLE_ENV:',
    '    use super::*;\n    #[cfg(windows)]\n    use crate::ProcessOwnershipVerification;\n\n    const ROLE_ENV:',
    1,
)
tree.write_text(s)

contained = Path('crates/medusa-runtime/src/analysis_contained.rs')
s = contained.read_text()
s = s.replace(
    'const UNIX_ANALYSIS_FILE_BYTES: u64 = 16 * 1024 * 1024;',
    '#[cfg(unix)]\nconst UNIX_ANALYSIS_FILE_BYTES: u64 = 16 * 1024 * 1024;',
)
contained.write_text(s)
