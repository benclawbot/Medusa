# medusa-daemon

`medusa-daemon` owns repository-scoped background jobs, reconnectable local IPC, durable job state, process ownership, and restart recovery.

## Transport

The public `DaemonClient` and wire protocol are the same on every supported platform.

- **Linux and macOS:** `.medusa/daemon/medusa.sock` is a Unix-domain socket.
- **Windows:** `.medusa/daemon/medusa.sock` is an endpoint descriptor containing an ephemeral loopback TCP address. The server binds only to the local loopback interface, and clients reject descriptors that resolve to a non-loopback address.

Every request uses a new connection. A client can disconnect and reconnect while a daemon-owned job continues running.

## Durable lifecycle

- Job records are persisted in `.medusa/daemon/jobs.json`.
- Queued or running jobs found after a daemon restart are marked `interrupted` with recovery evidence.
- The daemon records its PID in `.medusa/daemon/owner.pid` and reclaims stale ownership only when the recorded process is no longer alive.
- State replacement remains atomic on Unix. Windows uses a backup-and-restore swap because replacing an existing file with `rename` is not portable there.
- The endpoint descriptor or socket and owner record are removed during clean shutdown.

## Validation

The `Daemon` workflow runs formatting, Clippy, and reconnect/recovery tests on Ubuntu, macOS, and Windows. The tests exercise the same `DaemonClient` API and durable state semantics on all three systems.

## Remaining integration work

The daemon crate now provides cross-platform transport and recovery parity, but the terminal frontend still polls daemon jobs only on Unix. A following issue #42 increment must wire the TUI and desktop lifecycle to the same daemon contract on Windows, including daemon startup/restart ownership and user-visible connection state.

The daemon also still uses one synchronous thread per submitted job. Concurrency limits, backpressure, bounded queues, and shutdown semantics will be selected only after measured load and recovery tests; this transport change does not introduce an async runtime without evidence.
