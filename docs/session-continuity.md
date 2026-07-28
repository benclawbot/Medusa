# Cross-client session continuity

Medusa sessions use one presentation-neutral continuity record. The record is authoritative for task state, ordered events, attachments, ownership and handoffs; TUI and desktop presentation preferences remain client-local.

## Supported workflow

1. The first client attaches as the owner.
2. Another client may attach read-only and refresh from the authoritative revision.
3. The owner explicitly hands ownership to an attached client.
4. Only the current owner may mutate task state. Every mutation includes the last observed revision.
5. A stale client receives a revision conflict and must refresh. It never overwrites newer state.
6. Replaying the same event identity is idempotent. Reusing an identity for different content fails closed.

The shared contract preserves plan and active-step state, attention requirements, approval decisions, checkpoints, recovery state, verification evidence, file-change evidence and completion state. Attachment and handoff events are part of the audit timeline, while presentation-only preferences are not.

## Crash and partial-write behavior

Continuity records are written to a temporary file, synced and atomically renamed. A leftover temporary file from an interrupted write is ignored; the last authoritative record remains readable. Compatible schema-zero records migrate to the current schema during load. Newer unsupported schemas fail closed.

## Constraints

Only one client owns a session at a time. Additional clients attach read-only until an explicit handoff. Clients must supply the revision they last observed for every attach, handoff and mutation.
