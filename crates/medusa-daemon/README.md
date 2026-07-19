# medusa-daemon

`medusa-daemon` owns repository-scoped background jobs, reconnectable local IPC, durable job state, process ownership, restart recovery, bounded execution, per-job cancellation, graceful draining, and immediate process-tree shutdown.

## Transport and lifecycle ownership

The public `DaemonClient` and wire protocol are the same on every supported platform.

- **Linux and macOS:** `.medusa/daemon/medusa.sock` is a Unix-domain socket.
- **Windows:** the same path is an endpoint descriptor containing an ephemeral loopback TCP address and a fresh 256-bit launch capability. The server binds only to loopback, clients reject non-loopback descriptors, and every connection must present the capability before a request is read.
- The TUI and desktop use the same repository-scoped `DaemonSupervisor`, startup lock, hidden host mode, readiness check, and bounded restart backoff.
- Every request uses a new connection, so clients may disconnect while daemon-owned jobs continue.
- Local reads and writes have a five-second timeout, and requests are capped at 64 KiB.

## Bounded execution

The daemon uses a synchronous standard-library worker design rather than an async runtime:

- four concurrent job workers by default
- 32 queued jobs by default
- `daemon_busy` when the queue is full
- `serve_with_limits` and `spawn_with_limits` for deterministic embedding and tests
- no new operating-system thread per submission

The cross-platform load suite starts 64 simultaneous clients, verifies exact one-worker/one-queue backpressure, and confirms graceful shutdown drains accepted jobs. See [Daemon concurrency and backpressure](../../docs/DAEMON-CONCURRENCY.md).

## Cancellation and shutdown

The protocol keeps two intentionally different shutdown contracts:

- `Shutdown` is graceful: stop accepting requests, drain queued and running accepted jobs, join workers, then release endpoint ownership.
- `ShutdownNow` is immediate: remove queued jobs, terminate running process trees, persist interrupted state, then join workers.
- `Cancel { job_id }` removes queued work before execution or terminates the running job's process tree. Repeating cancellation for an already interrupted job is safe.

Every accepted job receives one process control before queue insertion, closing the race between queue removal, worker pickup, process spawn, and cancellation.

Process-tree handling is platform-specific without introducing unsafe Rust:

- Unix jobs start in isolated process groups and receive TERM followed by KILL escalation when needed.
- GNU/Linux commands delimit negative process-group IDs with `--`; `/proc` state inspection distinguishes terminated zombies from live descendants.
- macOS uses the same isolated process-group contract with native signal verification.
- Windows jobs start in isolated process groups and terminate through `taskkill /T /F`.
- If descendant termination fails, Medusa reports the platform error and does not silently claim success. Immediate-child kill is only a fallback.

Cancelled jobs remain persisted as `interrupted`, which keeps job history readable after rollback to binaries predating the additive cancellation requests.

## Durable lifecycle

- Job records are persisted in `.medusa/daemon/jobs.json`.
- Queued or running jobs found after an ungraceful restart are marked `interrupted` with recovery evidence.
- The daemon records its PID in `.medusa/daemon/owner.pid` and reclaims stale ownership only when the recorded process is no longer alive.
- State replacement remains atomic on Unix. Windows uses a backup-and-restore swap because replacing an existing file with `rename` is not portable there.

## Validation

The permanent `Daemon` workflow runs formatting, Clippy, daemon/TUI integration, reconnect and recovery, concurrent-client load, queue backpressure, queued cancellation, running descendant cancellation, unrelated-process isolation, and forced-shutdown tests on Ubuntu, macOS, and Windows.

The `Desktop` workflow validates the shared lifecycle adapter on all three platforms. Full Release Gates additionally enforce workspace coverage, adversarial regressions, fuzz and chaos checks, security policy, package smoke tests, and live MiniMax scenarios.

The request loop remains synchronous and bounded by the five-second I/O timeout. A connection-worker pool or async runtime should be considered only if measured frontend load exceeds the current 64-client acceptance threshold.
