# Issue 308: Windows sandbox boundary

The command API now fails closed on Windows and every platform without a verified containment backend. `sandboxed_command` never launches a bare child process. The structured `sandbox_unavailable` error includes the active backend and effective restrictions for diagnostics.

A separately named `unsandboxed_command` path requires explicit approval, clears the inherited environment, and is not used by the sandboxed path.

A native restricted-token and Job Object backend remains required before Windows commands can run through the sandboxed path.
