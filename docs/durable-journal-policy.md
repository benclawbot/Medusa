# Durable journal persistence policy

This document defines the durability boundaries for Medusa's canonical session journal. The journal remains the single execution authority; materialized session JSON, frontend state, and other projections are rebuildable views and must never publish authority ahead of a durable journal transition.

## Invariants

1. Authoritative transitions are appended before they can be published to frontends or treated as completed.
2. Frames remain ordered and hash chained. Grouping may reduce file opens and sync operations, but it may not reorder transitions.
3. A committed event and the snapshot/cursor that makes it replay-visible may share one ordered write batch and one file sync when both frames are written to the same journal before that sync returns.
4. A failed write or failed file sync fails the transition. Callers must not publish success after persistence failure.
5. Torn-tail recovery may discard only an incomplete final frame or a complete event tail that has no committed snapshot. Every earlier committed snapshot and its event prefix remain authoritative.
6. Materialized JSON snapshots are compatibility projections. They are written only after journal commitment and may be repaired from the journal.
7. Independent sessions must not serialize on one long-held global persistence lock.

## Durability matrix

| Transition | Journal append | Grouping | File sync | Directory sync | Snapshot timing | Publish timing | Crash/recovery rule |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Session creation / goal or plan authority | Required | Event + commit snapshot may share one ordered batch | Required before return | Required only when creating/replacing the journal file | Commit snapshot in the same durable batch | After durable batch | Recover last complete committed snapshot; discard torn/uncommitted tail |
| Mutation authorization / approval decision | Required | May batch only with its own commit snapshot; never with a later authority decision | Required before authority is usable | As above | Same durable batch | After sync | Missing/unsynced authorization is not authoritative |
| Integration decision / receipt | Required | May batch with its own commit snapshot | Required before integration is reported durable | As above | Same durable batch | After sync | Recovery cannot fabricate integrated success |
| Verification completion | Required | May batch with its own commit snapshot | Required before completion gate observes it | As above | Same durable batch | After sync | Recovery replays prior durable verification state |
| Cancellation / failure / terminal completion | Required | May batch with its own commit snapshot | Required before terminal state is published | As above | Same durable batch | After sync | Recovery chooses the last durable terminal/nonterminal state; never both |
| Checkpoint creation / restore authority | Required where represented in the canonical journal | Only with records whose order is already fixed | Required at the checkpoint authority boundary | As above | Projection after durable authority | After sync | Rebuild from journal/cursor and validate fingerprint |
| Tool/progress/frontend presentation with no authority change | Only when the event contract requires journal retention | Compatible records may be grouped when no authority boundary is crossed | Not independently required unless the group contains a durability boundary | No | Projection may lag | Never ahead of the durable cursor | Rebuild/re-deliver from the last durable cursor |
| Compatibility session JSON | No new authority | N/A | File sync before atomic replacement | Parent directory sync where supported | After canonical journal commitment | Not an authority publication | Missing/stale JSON is repaired from the journal |

## Crash points

The recovery contract is explicit at these points:

- **Before append:** no new authority exists.
- **During frame write:** the incomplete final frame is truncated; prior committed state survives.
- **After frame bytes, before file sync:** the transition is not acknowledged as durable; restart accepts only what validates as a complete committed prefix.
- **After file sync:** all frames in the ordered durability batch are eligible for authoritative replay.
- **Before compatibility snapshot:** journal state remains authoritative and the projection is rebuilt.
- **During compatibility snapshot replacement:** the temporary/projection file may be discarded or repaired; journal authority is unchanged.
- **Before frontend publication:** replay sees the durable transition even if no frontend observed it before the crash.

## Performance accounting

Representative benchmarks for issue #692 must report at least journal append batches, file syncs, serialized bytes, copied bytes where measurable, lock wait, snapshot/projection time, and persistence contribution to critical-path latency. The primary optimization target is synchronous operations per authoritative transition; correctness, replay fingerprints, and crash outcomes are hard gates rather than trade-offs.
