# Multi-agent execution

## Production model

Normal CLI, TUI, desktop, and runtime coding sessions use one authoritative agent loop. That loop owns the session plan, model turns, tool calls, repository mutations, verification, cancellation, and final response.

The production invariants are:

- one session has one mutation authority;
- repository tools enforce repository boundaries and transactional writes;
- cancellation stops the authoritative session and its owned processes;
- verification runs against the final repository state;
- no configuration field activates concurrent coding workers.

This is the only supported production execution model.

## Experimental transaction stack

The workspace contains research and test-focused crates for worker read sets, worker transactions, snapshots, scheduling, leases, commit barriers, rollback, replay, checkpoints, conflict resolution, transaction coordination, recovery, and consensus. The authoritative list is recorded in `workspace.metadata.medusa.experimental_multi_agent_crates` in the root `Cargo.toml`.

These crates are experimental building blocks. Being workspace members means they compile and are tested; it does not mean shipped entrypoints invoke them. They are not reachable from normal sessions and have no supported CLI, TOML, environment-variable, TUI, or desktop activation path.

The scheduler provides deterministic task ordering, capability matching, bounded worker capacity, dependency waves, retry state, worker-health handling, and exact write-path exclusion. Adjacent crates model leases, transactions, conflict handling, rollback, and recovery. Those component contracts are necessary but are not sufficient to claim a production multi-agent workflow.

## Ownership model required before promotion

A future production workflow must define and test all of the following as one end-to-end contract:

- **Planning:** one coordinator creates immutable task identities, dependencies, and declared read/write sets.
- **Files:** a lease or transaction owns every writable path; overlapping or ancestor/descendant writes are rejected before dispatch.
- **Tool calls:** workers receive bounded capabilities and cannot bypass the coordinator's repository and process policy.
- **Commits:** worker results enter through a commit barrier; unrelated working-tree changes are never included.
- **Conflicts:** deterministic conflict detection selects retry, serialization, or abort without silent last-writer-wins behavior.
- **Cancellation:** coordinator cancellation reaches every worker and descendant process, then releases leases.
- **Failure and recovery:** partial worker state is rolled back or replayed from durable checkpoints.
- **Verification:** verification runs after synthesis against the integrated repository state, not merely against isolated worker outputs.
- **Final synthesis:** only the coordinator may report completion, and only after integrated verification evidence exists.

Promotion requires shipped-entrypoint integration tests covering successful parallel work, overlapping writes, worker failure, cancellation, restart recovery, rollback, and synthesized-result verification on supported platforms.

## Configuration

`agent.parallel_workers` remains readable only for version-1 compatibility and currently has no production execution effect. It must not be treated as an experimental opt-in. A future production workflow must introduce an explicit, fail-closed activation contract only after the ownership and integration requirements above are implemented.
