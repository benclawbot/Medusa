# ADR 0007: Canonical frontend projection and cursor authority

- **Status:** Accepted
- **Date:** 2026-08-03
- **Issue:** #652

## Context

Medusa already had a versioned `FrontendCommandEnvelope` and `FrontendEventEnvelope`, but the deterministic projection from the canonical session journal lived under the Telegram adapter. The headless CLI, TUI, and desktop each consumed their own process-local `RuntimeEvent` shape. That allowed presentation order, terminal state, and replay behavior to diverge even though the journal was authoritative.

## Decision

`medusa-protocol::frontend` owns the only journal-to-presentation projection. The projector accepts the frontend kind so delivery identities remain frontend-scoped while payload, lifecycle, redaction, and canonical cursor semantics remain identical.

A `CanonicalFrontendEventStream` in `medusa-runtime` tails committed session events and exposes versioned frontend envelopes. Its cursor is the canonical journal sequence, including skipped non-presentable events, so reconnect and replay cannot reinterpret ordering.

The phase-6 migration order is enforced in reviewable slices:

1. headless CLI consumes the canonical stream;
2. TUI consumes the same stream while retaining only view-model conversion;
3. daemon IPC owns runtime commands, attachment, and replay for process-detachable clients;
4. desktop and remote frontends attach through that daemon authority;
5. direct frontend-owned runtime projections are deleted and guarded against reintroduction.

Telegram delivery consumes the same frontend-scoped replay envelopes as every other attached client. The Telegram adapter retains transport rendering and delivery state only; it no longer owns a journal projector.

## Migration status

The headless CLI and interactive TUI consume the canonical stream for durable transcript, plan, question, activity, usage, cancellation, failure, and completion state. Daemon attachments and replay project the same journal range according to each attached frontend kind and expose a next canonical cursor even when every scanned event is non-presentable. Telegram delivery consumes those daemon-projected envelopes directly and acknowledges the batch cursor after hidden events, rather than re-projecting raw journal payloads. Daemon protocol v2 exposes the shared frontend command envelope and typed acknowledgement through the repository-scoped local IPC server. Protocol v2 retains the existing durable-job request variants, but daemon request envelopes remain exact-version contracts: a v1 client must upgrade rather than having its payload silently reinterpreted. The desktop now consumes `FrontendKind::Desktop` envelopes for durable transcript, plan, activity, question, usage, cancellation, failure, and completion state. TUI and desktop keep process-local settings, startup recovery, turn-counter, reset hints, and desktop command execution only as bounded compatibility inputs while desktop commands and attachments move to daemon protocol v2.

## Consequences

- A frontend cannot report completion, cancellation, verification, or integration before the corresponding committed journal event exists.
- Replayed headless output uses the same redacted event contract as remote delivery.
- Presentation cursors are stable across process restarts and do not depend on how many event kinds a renderer suppresses.
- Existing daemon job operations continue under protocol v2, while mismatched wire versions fail closed before dispatch.
- Process-local runtime events remain temporary wakeups and compatibility inputs until the remaining phase-6 consumers migrate; they are not user-visible authority.
- A journal-publication failure is surfaced immediately through the transient fail-closed channel because, by definition, no canonical terminal event exists to replay.

## Rejected alternatives

- **Keep one projector per frontend:** rejected because redaction and lifecycle interpretation drift silently.
- **Project directly from transient `RuntimeEvent`:** rejected because it cannot provide durable replay or multi-client ordering.
- **Move presentation policy into the daemon only:** rejected because protocol-level tests must remain usable by local and remote frontends without depending on daemon internals.

## Removal criteria

Phase #652 is not complete until CLI, TUI, daemon, desktop, Telegram, and voice all consume the shared command/event authority by default, direct frontend-owned terminal-state inference is deleted, and cross-client replay/cancellation/approval equivalence passes on Linux, macOS, and Windows.
