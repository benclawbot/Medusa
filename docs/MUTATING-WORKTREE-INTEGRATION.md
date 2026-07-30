# Mutating Worktree Integration

Coordinated implementation must not edit the user's primary working tree directly.

## Production boundary

- The coordinator creates a dedicated Git branch and worktree for each mutating implementer.
- Each implementer runs an independent `AgentEngine` session with the runtime-enforced implementer tool policy.
- The worktree is created from the repository HEAD that the execution plan was built against.
- Changed paths are collected from Git and must remain within the task contract's allowed write scope.
- Verification runs inside the isolated worktree before any commit is accepted.
- Successful work is committed with deterministic Medusa author identity and persisted as durable evidence.
- The coordinator rejects overlapping worker paths before integration.
- Accepted commits are cherry-picked in stable worker order. Any conflict aborts and rolls the primary repository back to its pre-integration HEAD.
- Temporary worktrees and branches are removed after acceptance or rejection; durable evidence remains under `.medusa`.
- The user-facing parent session is read-only during coordinated execution. It reviews integrated evidence and cannot bypass worktree isolation with direct file writes.
- Final repository verification remains the only coordinated completion gate.

## Production status

Mutating execution is enabled only for objectives classified as repository modifications. Read-only coordinated analysis remains on the planner and researcher path and does not create an implementation worktree. Direct single-agent turns preserve their existing working-tree behavior.

## Acceptance proof

The implementation must prove:

1. isolated workers cannot see or overwrite each other's uncommitted changes;
2. out-of-scope changes are rejected before integration;
3. overlapping commits are rejected deterministically;
4. integration conflict rolls back every commit from the integration batch;
5. cancellation and worker failure preserve evidence and clean temporary resources;
6. successful integration removes worktrees and branches without deleting durable receipts;
7. direct single-agent mode retains its existing working-tree behavior.
