# medusa-daemon

`medusa-daemon` owns repository-scoped background jobs, reconnectable local IPC, durable job state, process ownership, restart recovery, bounded execution, and graceful shutdown.

## Transport

The public `DaemonClient` and wire protocol are the same on every supported platform.

- **Linux and macOS:** `.medusa/daemon/medusa.sock` is a Unix-domain socket.
- **Windows:** `.medusa/daemon/medusa.sock` is an endpoint descriptor containing an ephemeral loopback TCP address. The server binds only to the local loopback interface, and clients reject descriptors that resolve to a non-loopback address.

Every request uses a new connection. A client can disconnect and reconnect while a daemon-owned job continues running. Local reads and writes have a five-second timeout, and request bodies are capped at 64 KiB.

## Bounded execution

The daemon uses a synchronous standard-library worker design rather than an async runtime:

- four concurrent job workers by default
- 32 queued jobs by default
- `daemon_busy` response when the queue is full
- `serve_with_limits` and `spawn_with_limits` for deterministic test or embedding limits
- no new operating-system thread per submission

The permanent cross-platform load suite starts 64 simultaneous ping clients, verifies exact one-worker/one-queue backpressure, and confirms graceful shutdown drains accepted jobs. See [Daemon concurrency and backpressure](../../docs/DAEMON-CONCURRENCY.md).

## Durable lifecycle

- Job records are persisted in `.medusa/daemon/jobs.json`.
- Queued or running jobs found after a daemon restart are marked `interrupted` with recovery evidence.
- The daemon records its PID in `.medusa/daemon/owner.pid` and reclaims stale ownership only when the recorded process is no longer alive.
- State replacement remains atomic on Unix. Windows uses a backup-and-restore swap because replacing an existing file with `rename` is not portable there.
- Graceful shutdown stops new request acceptance, drains accepted queued and running jobs, joins the fixed workers, then removes the endpoint and owner record.

## Validation

The `Daemon` workflow runs formatting, Clippy, daemon/TUI integration, reconnect/recovery, concurrent-client, backpressure, and shutdown tests on Ubuntu, macOS, and Windows. The tests exercise the same `DaemonClient` API and durable state semantics on all three systems.

## Remaining integration work

The daemon transport, recovery, bounded job execution, and TUI observation paths are cross-platform. One shared external lifecycle owner for TUI and desktop is still required, including executable discovery, startup race handling, restart policy, coordinated shutdown, and visible degraded/recovery states.

Graceful shutdown currently waits for running child processes to finish. Medusa does not yet forcibly terminate a hung process tree because cross-platform descendant-safe cancellation requires a separate lifecycle and process-group design. The request loop also remains synchronous, bounded by the five-second I/O timeout; a connection worker pool or async runtime should be considered only if measured frontend load exceeds the current 64-client acceptance threshold.
