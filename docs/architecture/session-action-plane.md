# Durable session action plane

The canonical session journal is the only durable authority for operator actions that arrive while a session is active. There is no separate durable follow-up, replacement, or steering queue.

## Action contract

Every action carries a stable `action_id`, idempotency key, source/provenance, target session, expected journal revision, typed action kind, delivery policy, wake policy, and payload. Admission, rejection, lifecycle changes, supersession, transcript linkage, and terminal outcomes are reconstructed from journal events.

Supported action kinds are:

- `follow_up`: deliver only when the runtime is idle.
- `replace_follow_up`: atomically supersede one still-queued follow-up and deliver the replacement when idle.
- `steer`: defer the instruction to the next safe turn boundary.
- `goal_adjustment`: update the authoritative goal at its declared safe boundary.
- `cancel`: durably record cancellation before signalling the active runtime.

## CAS and replacement

Action admission is serialized inside the journal write lock. The supplied `expected_session_revision` must equal the committed journal cursor at that serialization point. An exact duplicate action/idempotency identity replays its existing admission. A conflicting identity fails closed. A stale revision is appended as a durable rejected action with the authoritative revision and reason.

`replace_follow_up` additionally names `replaces_action_id`. The journal accepts the replacement only if the target is still a queued `follow_up` or `replace_follow_up` at the same serialized admission point. The accepted replacement makes the target terminal with `superseded` evidence. This makes replace-versus-enqueue races deterministic: one same-revision writer wins journal CAS and every loser remains auditable.

## Safe-boundary steering

A steering action never mutates an in-flight tool step. While work is active, the accepted steer is journaled and queued using the existing canonical follow-up boundary. No action lifecycle transition into delivery and no authoritative transcript linkage occurs until the runtime reaches the boundary where the current tool step has finished and the queued follow-up is dequeued for the next provider turn.

The delivery lifecycle is monotonic:

`queued -> selected -> preparing -> committing -> running -> completed`

Failure or cancellation may terminate only from the explicitly allowed intermediate states. `committing` never rolls back to `queued`; restart recovery must either prove the committed transcript linkage and continue forward or record a terminal failure.

## Restart and frontend projection

Restart/reconnect reloads the action projection from the canonical journal. Superseded, rejected, failed, cancelled, and completed actions remain in history but are not recovered for delivery. Only surviving nonterminal actions are recoverable, so a pending replacement reconstructs as exactly one next action.

Desktop, TUI, Telegram, headless, and other frontends consume the same canonical journal projection. Frontend-specific event identifiers may differ by suffix, but action state, lifecycle, rejection reason, authoritative revision, and final outcome are derived from the same journal records.
