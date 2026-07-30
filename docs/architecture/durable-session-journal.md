# Durable session journal foundation

Issue #569 consolidates recovery, replay, checkpoint, and frontend continuity around one canonical runtime history. `RuntimeController` remains the sole product/session authority, while `medusa_protocol::EventEnvelope` remains the canonical event contract.

## Record model

Each session has one framed append-only journal at `.medusa/journals/<session-id>.events`, with the existing per-repository fallback location used when repository-local state cannot be written. The file begins with a versioned magic header. Every following frame contains either:

- an integrity-protected `EventEnvelope` write-ahead record; or
- a committed materialized `AgentSession` snapshot bound to the exact preceding event cursor and checksum.

A normal persistence boundary is:

1. validate and append each event record, then flush it with `sync_data`;
2. mutate the in-memory session;
3. append a full snapshot commit record bound to the complete event prefix, then flush it;
4. atomically rewrite and flush the compatibility session JSON.

The journal snapshot is authoritative over the compatibility JSON because it is committed first. State-only mutations are also committed as snapshot records, so recovery is not limited to fields represented by event payloads. Event records are boxed inside the record enum to keep the parser's stack footprint bounded without changing serialization.

## Crash behavior

On load, Medusa validates every complete frame, event checksum, event ID, sequence, previous-hash link, snapshot cursor, and snapshot checksum.

- A torn final frame is truncated to the last complete frame.
- Complete event records after the last snapshot are treated as an uncommitted operation tail and are discarded back to the last committed snapshot.
- A committed journal snapshot newer than the JSON snapshot repairs the JSON snapshot.
- A valid legacy JSON snapshot with no journal is migrated without changing existing events.
- Divergent committed histories fail closed.

Existing patch-transaction recovery remains responsible for reconciling external repository mutations that may have occurred during an uncommitted operation.

## Replay contract

Replay uses zero-based committed cursors. A cursor can return only events included in the latest committed snapshot; an out-of-range cursor fails closed. Session discovery includes journal-only sessions, allowing a committed session to repair a missing compatibility snapshot.

## Retained components

This foundation does not restore the removed lifecycle facade and does not remove any retained component. `medusa-execution-checkpoint`, `medusa-execution-replay`, `medusa-time-travel`, `medusa-session-continuity`, and all compatibility paths remain present. Later #569 slices will connect them to the committed journal and `RuntimeController`; any proposed deletion requires explicit approval before implementation.

The repository CI matrix is authoritative for formatting, compilation, linting, tests, documentation, and cross-platform behavior of this foundation.
