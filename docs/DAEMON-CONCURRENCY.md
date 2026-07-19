# Daemon concurrency and backpressure

## Decision

Medusa keeps a synchronous standard-library daemon architecture. The evidence does not justify introducing Tokio or Mio as direct runtime dependencies.

Before bounded execution, the daemon handled local requests serially and created one operating-system thread for every submitted job. There was no queue limit, no busy response, clean shutdown did not join daemon-owned workers, and child execution could not be cancelled safely.

The current design separates short local requests from background process execution:

- request parsing remains synchronous and repository-local
- background execution uses a fixed worker set
- accepted waiting jobs use a bounded queue
- excess submissions receive `Response::Error` with code `daemon_busy`
- IPC reads and writes have finite timeouts
- request bodies have a fixed maximum size
- one cancellation control is registered before each accepted job enters the queue
- graceful shutdown drains accepted work
- immediate shutdown cancels queued and running work before worker join

## Production defaults

`DaemonLimits::default()` configures:

| Limit | Default |
|---|---:|
| Concurrent job workers | 4 |
| Queued jobs | 32 |
| Maximum request body | 64 KiB |
| Local IPC read/write timeout | 5 seconds |

`serve_with_limits` and `spawn_with_limits` allow deterministic test and embedding configurations. Zero workers or zero queue capacity are rejected as invalid configuration.

## Cross-platform load and cancellation evidence

The permanent `Daemon` workflow executes the daemon and TUI suite on Ubuntu, macOS, and Windows. The suite requires:

1. **64 simultaneous ping clients** — all clients cross a barrier together and receive `Pong` through the reconnecting `DaemonClient` API.
2. **Exact worker and queue limits** — with one worker and one queue slot, one job is running, one remains queued, and a third submission receives `daemon_busy` without retaining a durable record.
3. **Graceful shutdown** — `Shutdown` stops request acceptance, drains accepted jobs, joins workers, and persists successful terminal records.
4. **Queued cancellation** — a cancelled queued command never starts and is persisted as `interrupted`.
5. **Running process-tree cancellation** — cancelling a running job terminates descendants within the bounded test interval while an unrelated process remains alive.
6. **Immediate shutdown** — `ShutdownNow` cancels queued and running work and returns without waiting for the original long-running child duration.
7. **Restart readability** — interrupted records remain valid durable state and require no migration.

These tests are acceptance thresholds, not throughput claims.

## Process ownership and cancellation

Cancellation is deliberately implemented around operating-system process ownership rather than an async executor:

- Unix jobs start in isolated process groups and receive TERM/KILL escalation.
- GNU/Linux uses `--` before negative process-group IDs so the external `kill` utility cannot interpret the group ID as an option.
- Linux `/proc` inspection treats running or stopped members as live while ignoring already terminated zombie entries awaiting reaping.
- macOS retains process-group signal verification.
- Windows jobs start in isolated process groups and terminate through `taskkill /T /F`.

The implementation reports descendant-termination failures instead of silently treating an immediate-child kill as complete tree cancellation. No unsafe Rust, new dependency, or persisted-state migration was introduced.

## Why no async runtime

The protocol is local, line-oriented, short-lived, and uses one connection per request. Background child processes dominate job duration; async sockets would not remove that process cost. A fixed worker set, bounded queue, explicit process controls, and platform process-tree termination directly address the measured resource and shutdown risks.

An async or multiplexed connection architecture should be reconsidered only if measured workloads show one of these failures:

- the 64-client burst threshold becomes insufficient for real frontend usage
- request timeout frequency is material under normal local load
- connection parsing becomes CPU-bound
- frontend integration requires long-lived streaming connections

## Remaining uncertainty

The request loop is still synchronous. A slow or malformed client can occupy it only until the five-second I/O timeout, and an oversized request is rejected after 64 KiB. A bounded connection-worker pool is the next escalation step only if load evidence shows the current boundary is insufficient.