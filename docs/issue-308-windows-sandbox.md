# Issue 308: Windows sandbox boundary

The command API now fails closed on Windows and every platform without a verified containment backend. `sandboxed_command` never launches a bare child process. The structured `sandbox_unavailable` error includes the active backend and effective restrictions for diagnostics.

No unsandboxed fallback is exposed by the sandbox API. Unsupported platforms return before process launch.

A focused platform-gated test verifies that unsupported platforms return `SandboxUnavailable` before process launch. Linux bubblewrap and macOS Seatbelt paths remain unchanged and continue through the repository's cross-platform CI. The helper that constructs the structured error is compiled only on platforms that use this fail-closed path.

A native restricted-token and Job Object backend remains required before Windows commands can run through the sandboxed path.