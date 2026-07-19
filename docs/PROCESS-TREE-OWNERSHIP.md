# Process-tree ownership and bounded shutdown

Status: corrective implementation contract for issue #69 and draft PR #70.

## Current gap

The daemon now has bounded cancellation machinery, but the Windows implementation still uses `taskkill /T /F`, verifies liveness with `tasklist`, and can fall back to killing only the immediate child. Those choices do not provide creation-time containment and can leave descendants or misreport cleanup.

## Required invariant

Medusa must not report cancellation or daemon shutdown completion while any process in a tree it launched remains outside a retained operating-system containment boundary.

## Lifecycle

1. Create the containment boundary before user code can execute.
2. Start the direct child inside that boundary.
3. Retain child and containment handles for the complete execution.
4. Capture output without allowing a full pipe to deadlock execution.
5. On cancellation, request graceful whole-tree termination where supported.
6. After a bounded grace interval, forcefully terminate the complete tree.
7. Reap the direct child and release all handles before reporting completion.

## Unix

- Start each command in a dedicated process group or session.
- Retain the group identifier and ensure it cannot target Medusa's own group.
- Signal the complete group for graceful and forced termination.
- Treat an absent group as already stopped.
- Reap the direct child and verify no live group member remains.

External `kill` invocation should be replaced with direct platform signaling when practical so cleanup does not depend on launching another process during shutdown.

## Windows

- Create one Job Object for every launched command tree.
- Configure `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
- Create the initial process suspended.
- Assign the process to the Job Object before any user code runs.
- Resume only after successful assignment.
- Retain the Job Object, process, and thread handles for the complete execution.
- Forceful cleanup uses `TerminateJobObject`.
- Never use `taskkill`, `tasklist`, process-name matching, or descendant PID enumeration.
- Never report direct-child fallback as successful tree cleanup.

A spawn-then-assign race is not accepted as the final design.

## Cancellation semantics

Cancellation is idempotent and scoped to one retained containment object. Concurrent cancellation requests must never cross-signal another job. A cleanup error remains an error even when the direct child was subsequently killed.

## Result semantics

Results distinguish normal exit, non-zero exit, graceful cancellation, forced whole-tree termination, timeout escalation, daemon-shutdown escalation, and cleanup failure.

No persisted job-schema migration is required unless implementation evidence proves that these distinctions must be durable outside existing result text.

## Acceptance tests

- ordinary short-lived command preserves status and output
- cancellation is idempotent
- concurrent jobs cannot cross-signal
- large output does not deadlock
- direct-child exit with a live descendant is not reported as cleanup success
- Unix descendant ignoring graceful termination is removed after escalation
- Windows child and long-lived grandchild are removed through one Job Object
- Windows process cannot execute before Job Object assignment
- repeated Windows execution returns native handle usage to baseline
- daemon shutdown remains bounded with multiple hung trees
- daemon, TUI, desktop, persistence, and recovery suites pass on Ubuntu, macOS, and Windows

## Alternatives rejected

- immediate-child kill fallback
- `taskkill /T`
- `tasklist` liveness checks
- PID or process-name scanning
- Unix-only containment
- platform termination code duplicated in scheduler or server

## Risk and rollback

Risk is medium-high because process creation and termination are security boundaries. Reverting the corrective PR restores the previous behavior without data migration, but documentation must continue to disclose that Windows cleanup is not safely contained until Job Object ownership lands.