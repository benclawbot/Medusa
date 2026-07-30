# Durable live-session attachments

Medusa frontends attach to one authoritative journal-backed session through `medusa_runtime::LiveSessionBroker`. The broker is owned by `RuntimeController`; it does not create a second runtime, transcript, task graph, or event stream.

## Ownership boundary

The canonical execution history remains the crash-durable session journal. The continuity store persists only frontend coordination state:

- attached client identity and frontend kind;
- owner versus read-only mode;
- revision-checked attach, detach, and ownership handoff events;
- each client's monotonic acknowledged journal cursor.

A client attachment never copies messages, plans, approvals, tasks, or completion truth into the continuity store.

## Replay and cursors

Attach accepts a zero-based committed journal cursor. The broker validates the session against the latest committed journal snapshot and returns every canonical event after that cursor. The returned `next_cursor` is the exact committed position following the replay batch.

Cursor acknowledgement is persisted only for an attached client, cannot move backwards, and cannot advance beyond the committed journal tail. Replayed attach, detach, handoff, and acknowledgement commands retain the continuity store's existing idempotency and conflicting-event rejection semantics.

## Frontend behavior

TUI, desktop, Telegram, headless, and future frontends use the same broker operations exposed by `RuntimeController`:

- list and inspect durable sessions;
- attach as owner or read-only observer;
- replay canonical events from a durable cursor;
- acknowledge delivered cursors;
- detach without cancelling the runtime;
- hand ownership to another attached client with optimistic revision checks.

Telegram chat/topic binding and transport message identifiers remain adapter-owned presentation state. They reference this broker contract rather than becoming runtime authority.
