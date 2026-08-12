# Issue 308: native Windows command sandbox

> Historical record — retained as implementation evidence; it is not current setup or status guidance. Start at [the documentation index](README.md).

Windows command execution now uses an AppContainer process identity with no network capabilities. The launcher grants the AppContainer SID access only to the repository working tree and the selected executable, creates the process suspended, assigns it to a kill-on-close Job Object, and resumes it only after containment is established.

The child receives an explicit environment allowlist containing `PATH`, `SystemRoot`, repository-scoped temporary directories, and Medusa sandbox diagnostics. Arbitrary inherited variables and secrets are excluded.

Every profile, ACL, process-attribute, pipe, process-launch, Job Object, and resume failure returns `SandboxUnavailable`; there is no bare-process downgrade. The active backend and effective restrictions are included in structured error context.

The Job Object prevents breakaway by omission of breakaway flags, limits active processes and aggregate committed memory, and terminates the process tree when its final handle closes. AppContainer execution has no network capabilities, so outbound and private-network access are denied by Windows.

All Windows FFI is isolated in `medusa-process-containment`. The `medusa-agent` policy boundary calls a safe API and retains the workspace-wide `unsafe_code = "forbid"` guarantee.

The launcher uses the Windows SDK `ReadFile` binding from `Win32_Storage_FileSystem` and passes immutable security attributes to `CreatePipe` as required by the generated binding. Both contracts are compiled by the authoritative Windows matrix rather than inferred from non-Windows builds.

Validation covers the complete repository suite: dependency graph and lockfile policy, formatting, Clippy, panic audit, workspace tests, documentation, refactor guardrails, and the Windows, macOS, and Ubuntu daemon/TUI matrix. All failed jobs and their diagnostics are collected before corrective changes are made, and the authoritative validation runs from a normal branch commit.
