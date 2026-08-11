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
base.write_text(s)

tree = Path('crates/medusa-process-containment/src/process_tree.rs')
s = tree.read_text()
s = s.replace(
    '#[cfg(any(unix, windows))]\nuse crate::{ProcessOwnershipReceipt, ProcessOwnershipVerification};',
    '#[cfg(any(unix, windows))]\nuse crate::ProcessOwnershipReceipt;\n#[cfg(unix)]\nuse crate::ProcessOwnershipVerification;',
)
tree.write_text(s)
