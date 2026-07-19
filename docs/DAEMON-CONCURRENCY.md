# Daemon concurrency and backpressure

## Decision

Medusa keeps a synchronous standard-library daemon architecture for this increment. The evidence did not justify introducing Tokio or Mio as direct runtime dependencies.

Before this change, the daemon handled local requests serially and created one operating-system thread for every submitted job. There was no queue limit, no busy response, and clean shutdown did not join daemon-owned job workers.

The bounded design now separates short local requests from background process execution:

- request parsing remains synchronous and repository-local
- background execution uses a fixed worker set
- accepted waiting jobs use a bounded queue
- excess submissions receive `Response::Error` with code `daemon_busy`
- IPC reads and writes have finite timeouts
- request bodies have a fixed maximum size
- shutdown stops the listener, drains accepted jobs, joins workers, and then removes endpoint ownership

## Production defaults

`DaemonLimits::default()` configures:

| Limit | Default |
|---|---:|
| Concurrent job workers | 4 |
| Queued jobs | 32 |
| Maximum request body | 64 KiB |
| Local IPC read/write timeout | 5 seconds |

`serve_with_limits` and `spawn_with_limits` allow deterministic test and embedding configurations. Zero workers or zero queue capacity are rejected as invalid configuration.

## Cross-platform load evidence

`crates/medusa-daemon/tests/concurrency_limits.rs` is executed by the permanent `Daemon` workflow on Ubuntu, macOS, and Windows.

The suite requires:

1. **64 simultaneous ping clients** — all clients cross a barrier together and must receive `Pong` through the normal reconnecting `DaemonClient` API.
2. **Exact worker and queue limits** — with one worker and one queue slot, one job must be `running`, one must remain `queued`, and the third submission must receive `daemon_busy` without creating a durable job record.
3. **Graceful shutdown** — a protocol `Shutdown` request must stop new request acceptance, drain both accepted jobs, join the worker, and persist both records as `succeeded`.

These tests are acceptance thresholds, not throughput claims. They establish that the current synchronous request loop handles a meaningful local burst while job execution remains bounded.

## Why no async runtime

The daemon protocol is local, line-oriented, short-lived, and uses one connection per request. Background child processes dominate job duration; async sockets would not remove that process cost. A fixed worker set and queue directly address the observed unbounded resource risk with no new dependency graph or executor lifecycle.

An async or multiplexed connection architecture should be reconsidered only if measured workloads show one of these failures:

- the 64-client burst threshold becomes insufficient for real frontend usage
- request timeout frequency is material under normal local load
- connection parsing becomes CPU-bound
- desktop and TUI lifecycle integration requires long-lived streaming connections
- process cancellation requires an asynchronous child-management design

## Shutdown semantics and remaining uncertainty

Shutdown is graceful for accepted work: queued jobs drain and running child processes are allowed to finish before worker threads join. This prevents accepted jobs from silently disappearing and keeps persisted state consistent.

The daemon does not yet forcibly terminate a long-running child process. A hung child can therefore delay graceful shutdown. Cross-platform process-group cancellation and a bounded forced-shutdown policy require a separate design because terminating only the immediate child can orphan descendants. That limitation must remain visible until lifecycle ownership and cancellation semantics are implemented for both TUI and desktop clients.

The request loop is still synchronous. A slow or malformed client can occupy it only until the five-second I/O timeout, and an oversized request is rejected after 64 KiB. A bounded connection-worker pool is the next escalation step if load evidence shows the timeout boundary is not sufficient.
