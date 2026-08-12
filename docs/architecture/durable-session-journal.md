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

Runtime restart recovery uses this same boundary when interrupted plan steps are marked failed; it never updates only the compatibility JSON.

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

The repository CI matrix is authoritative for formatting, compilation, linting, tests, documentation, and cross-platform behavior of this foundation. The focused journal tests cover stale snapshot repair, uncommitted-tail recovery, torn writes, checksum corruption, legacy migration, duplicate IDs, state-only commits, and cursor replay.

## Runtime controller event coverage

Protocol version 1.1 adds canonical payloads for controller-owned transitions that were previously available only through the in-process `RuntimeEvent` stream. The controller and agent now commit authoritative state before publishing the corresponding frontend event.

The production ordering is:

1. mutate the authoritative session/controller state;
2. append the canonical `EventEnvelope` and a snapshot commit under the journal write lock;
3. update the compatibility snapshot;
4. publish the derived `RuntimeEvent` or `AgentUpdate`.

The write lock also reconciles a stale in-memory session with controller events committed by another runtime thread before assigning the next sequence and previous-event hash. A controller event can therefore be inserted while the model loop is active without forking the hash chain or losing the event at the next snapshot boundary.

Canonical controller payloads now cover:

- production execution-plan creation;
- queued follow-up acceptance and dequeue;
- structured question and approval request/decision state;
- assistant messages and visible plan changes;
- tool execution start and completion around the actual action;
- team/worker lifecycle snapshots and coordinator evidence;
- mutating-worker integration receipts;
- verification start/results through the existing agent event path;
- recovery action receipts;
- cancellation request/completion;
- runtime turn completion, terminal failure, and session reset.

Queued follow-ups are reduced from journal events during resumed startup. A queued command remains pending until its matching dequeue event. Explicit cancellation completion, terminal runtime failure, session reset, or session completion clears the pending queue. This makes a crash after queue acceptance but before dequeue recoverable without duplicating a command that already began execution.

Every shipped `RuntimeEvent` has an explicit durability classification:

| Runtime event | Classification | Durable source |
| --- | --- | --- |
| `RecoveryAvailable` | durable projection | recovery coordinator record |
| `RecoveryCompleted` | canonical | `recovery_action_completed` |
| `Started` | presentation-only | frontend busy indicator |
| `AssistantText` | canonical | `assistant_message_recorded` |
| `Activity` | presentation-only projection | model/tool/verification/worker events where applicable |
| `Team` | canonical | `team_state_changed` |
| `Plan` | canonical | `plan_updated` |
| `Question` | canonical | `question_requested` |
| `Usage` | canonical | `model_response_received` |
| `Progress` | durable projection | committed materialized session turn |
| `Settings` | presentation-only | process-local frontend settings |
| `Notice` | presentation-only | human-readable operational notice |
| `NewSession` | presentation-only | follows committed `session_reset` when a session exists |
| `Compacted` | canonical | `conversation_compacted` |
| `Completed` | canonical | `session_completed` |
| `TurnFinished` | canonical | `runtime_turn_finished` |
| `Cancelled` | session-bound canonical | `cancellation_completed`; pre-session cancellation is explicitly classified |
| `Failed` | session-bound canonical | `runtime_failed`; startup failure before session creation is explicitly classified |

The central runtime dispatcher persists controller-owned canonical events before forwarding them. If serialization or journal persistence fails, the authoritative event is not published as though it succeeded. The failed authoritative event is suppressed; only an explicit persistence failure notification is forwarded. Agent-owned events use the same append-and-commit boundary before observers can derive frontend updates.

The low-level `append_event`, `AppendDisposition`, and `commit_snapshot` paths remain retained for compatibility and explicit idempotency/conflict handling; production event creation and full persistence now use the committed append transaction and atomic publication boundary. Snapshot commits accept an exact existing write-ahead tail, merge newer committed controller events into a stale session, and discard only an unrelated crash tail before failing closed on genuine divergence.

Concurrent full-session persistence now holds the same journal lock through compatibility-snapshot publication and uses collision-free temporary files, preventing competing writers from renaming the same temporary path. The regression suite exercises repeated multi-threaded persistence and verifies that no temporary files remain after publication.

This slice remains additive. It does not remove or consolidate `medusa-execution-checkpoint`, `medusa-execution-replay`, `medusa-time-travel`, `medusa-session-continuity`, or any compatibility path. Production checkpoint materialization and deterministic state replay remain the next dependent phases after this authoritative stream is validated.
