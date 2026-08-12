# Runtime durability, replay, and recovery

Medusa has one authoritative execution history: the session-scoped, hash-chained `EventEnvelope` journal owned by the production runtime path. Materialized session JSON, persisted checkpoints, recovery projections, historical views, and frontend attachment state are derived from that journal and must verify against it before use.

## Storage layout

Repository-local durable state lives below `.medusa/`:

- `sessions/` contains the materialized `AgentSession` snapshots and canonical event history used for compatibility and startup.
- `journal/` contains the independently durable session event stream and cursor index where enabled by the session store.
- `checkpoints/<session-id>/` contains verified, journal-anchored runtime checkpoint records.
- `recovery-checkpoints/` contains bounded repository payloads for explicit checkpoint restore.
- `recovery/` contains recovery-coordinator projections derived from verified checkpoints.
- `continuity/` contains client attachment metadata: owner, read-only attachments, revisions, and acknowledged journal cursors.

A missing derived artifact may be rebuilt from the canonical journal. A derived artifact that conflicts with the journal fails closed.

## Durability ordering

Authoritative transitions are recorded before the corresponding live presentation is treated as accepted. Safe transition boundaries materialize a checkpoint artifact, sync it, and only then append the matching `CheckpointCreated` event. Atomic temporary-file replacement and parent-directory synchronization are used for checkpoint and continuity records on supported platforms.

The journal provides stable event identities, monotonic sequence/cursor values, checksums, and previous-event hashes. Duplicate identities with identical content are idempotent; conflicting reuse, broken chains, modified payloads, and unsupported schema versions are rejected.

## Resume, historical view, and restore

These operations are deliberately distinct:

- **Resume latest** verifies the materialized session against the canonical journal, restores the latest valid runtime state, and continues without re-running completed provider or tool work.
- **Historical view** reduces verified journal events to a requested cursor and is read-only.
- **Restore checkpoint** loads a verified checkpoint and bounded repository payload, generates an exact preview, rejects stale preflight evidence, then applies repository changes transactionally. Restore is recorded as new lineage; prior history is never erased.

Unsupported, oversized, non-UTF-8, non-file, traversal, or symlink-escaping payloads block destructive restore. Failed application attempts roll back already changed files where possible and report incomplete rollback as a hard failure.

## Multi-client continuity

TUI, desktop, Telegram, daemon, and future frontends attach to the same session and replay the same canonical journal. Continuity storage does not contain a second transcript or task state. It records only client attachment metadata needed for one mutable owner, read-only observers, explicit handoff/detach, revision conflicts, idempotent commands, and durable cursor acknowledgement.

Clients reconnect by attaching with their last acknowledged cursor. They receive only the canonical tail after the maximum of the requested cursor and the durable acknowledged cursor. A client cannot silently switch sessions, regress its cursor, or mutate through a stale revision.

## Redaction and retention

Durable events and recovery artifacts contain safe structured summaries, fingerprints, receipts, and evidence references. Credentials, OAuth tokens, Telegram bot tokens, hidden model reasoning, live process handles, channels, and unrestricted private tool inputs are not recoverable state and must not be persisted.

Retention must preserve the journal ancestry and evidence required by every retained checkpoint. Presentation-only preferences may be removed independently because they are not authoritative execution state.

## Diagnostics

`medusa doctor` verifies every discoverable session journal by replay, reports total verified cursors, validates all persisted checkpoint records, and identifies the latest recoverable checkpoint. Corruption, incompatible schemas, conflicting fingerprints, malformed continuity metadata, or replay divergence produce a failed check with an actionable error while leaving the prior valid state untouched.

`medusa health --json` is the bounded operational projection for startup and support workflows. It
aggregates typed component evidence, marks optional routes as unavailable until live readiness is
proven, applies backpressure status before the state budget is full, and returns a non-zero result
for recovery-required or unsafe states. `medusa health --support-bundle PATH` creates a local,
versioned JSON bundle with bounded lifecycle events and an explicit excluded-data manifest; it does
not probe providers, launch processes, upload data, or replace the canonical journal.
