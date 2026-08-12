# Cross-client session continuity

Medusa frontends share one canonical session journal and one production runtime authority. The continuity record is deliberately narrower: it stores client attachment metadata needed to coordinate ownership, read-only observation, revision conflicts, handoffs, detach, and durable cursor acknowledgement. It is not a second transcript or task-state authority.

## Supported workflow

1. The first client attaches as the mutable owner.
2. Additional TUI, desktop, Telegram, daemon, or future clients attach read-only to the same session.
3. Each attachment replays canonical journal events after the maximum of its requested cursor and its durable acknowledged cursor.
4. A client acknowledges only monotonically increasing cursors.
5. The owner may explicitly hand ownership to another already attached client.
6. A client may detach without cancelling or modifying the session.
7. Every continuity mutation includes the last observed revision. A stale client must refresh and retry; it never overwrites newer metadata.
8. Replaying the same event identity with identical content is idempotent. Reusing an identity for different content fails closed.

The canonical journal and materialized `AgentSession` preserve objectives, messages, plans, questions, approvals, tool and worker evidence, checkpoint lineage, verification, cancellation, recovery, and completion state. Continuity records reference the session and client cursor but do not duplicate those fields as authoritative state.

## Ownership and runtime resume

Only one attached client may own mutable control where ownership is required. Read-only clients observe the same event stream and final state. Ownership changes are explicit; attaching a new client never silently steals control or forks a transcript.

An owner attachment may be transferred into the existing `RuntimeController` resume path. Resume verifies journal/session equivalence before accepting commands. A client already bound to one session must detach before attaching to another.

## Crash and partial-write behavior

Continuity records are written to a temporary file, synced, and atomically renamed. A leftover temporary file from an interrupted write is ignored, leaving the last valid record readable. Compatible older records migrate by adding safe defaults such as a zero acknowledged cursor. Newer unsupported schemas, malformed attachment metadata, cursor regression, conflicting event identities, and revision mismatches fail closed.

A daemon or frontend restart does not require reconstructing a parallel transcript. The client reloads continuity metadata, attaches to the canonical session, and replays the durable journal tail.

## Presentation state

Frontend layout, Telegram formatting, local panel state, typing indicators, streaming preview message IDs, and similar presentation preferences remain client-local. They may use journal cursors for idempotent delivery but never become execution authority.

See [Runtime durability, replay, and recovery](EXECUTION-DURABILITY.md) for journal, checkpoint, restore, retention, and diagnostic guarantees.
