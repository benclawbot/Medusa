# Durable session journal foundation

Issue #569 consolidates recovery, replay, checkpoint, and frontend continuity around one canonical execution history. The first production slice keeps `RuntimeController` as the product authority and makes the existing `medusa_protocol::EventEnvelope` chain crash durable.

## Storage contract

Each session has one framed append-only journal at `.medusa/journals/<session-id>.events`, with the existing per-repository fallback location used when repository-local state cannot be written. The file begins with a versioned magic header. Every following record contains a fixed-width length followed by one JSON-encoded `EventEnvelope`.

The append order is:

1. validate the event checksum, session ID, monotonic sequence, and previous hash;
2. append the framed record;
3. flush the journal record with `sync_data`;
4. update the in-memory materialized session;
5. atomically rewrite the compatibility session snapshot.

A crash after step 3 and before step 5 leaves a valid journal tail. Session loading replays that tail into the materialized snapshot and rewrites the snapshot with the applied cursor and final checksum.

## Compatibility and recovery

Existing `.medusa/sessions/*.json` files remain supported. On first load, their validated `events` chain is migrated into the journal without changing event IDs, sequences, timestamps, payloads, or checksums.

Journal loading rejects:

- an unsupported or missing file header;
- invalid record lengths;
- complete records with invalid JSON or checksums;
- session-ID mismatches;
- non-monotonic sequences;
- broken previous-hash links;
- duplicate or conflicting event IDs;
- divergence between a materialized snapshot and the canonical journal prefix.

Only an incomplete final frame is treated as a torn write. The invalid tail is truncated to the last complete validated record and prior events are preserved.

## Authority boundary

`AgentSession` remains the compatibility materialized snapshot, not an independent source of truth. It records `applied_journal_cursor` and `applied_journal_checksum`, and persistence rejects a snapshot whose binding does not match its event chain.

This slice does not restore the removed lifecycle facade and does not make the test-only checkpoint, replay, time-travel, or continuity stores authoritative. Later #569 slices must consume this journal from `RuntimeController`, add durable presentation events and cursor subscriptions, and either production-wire or remove the remaining overlapping stores.
