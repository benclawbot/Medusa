# ADR 0005: Transactional mutation, review, verification, and integration

- Status: Accepted
- Date: 2026-08-02
- Issue: #649

## Context

The production mutating coordinator currently finalizes an isolated worker commit and immediately cherry-picks it into the primary repository. Parent review and repository verification run afterward. This ordering means a rejected review or failed verification can observe a repository that has already changed, and restart recovery must infer whether integration happened before the durable state write.

## Decision

Every production mutation will use one durable transaction with the following lifecycle:

`Planned -> PreparedInIsolation -> ReviewPending -> RevisionRequested|ReviewAccepted -> VerificationPending -> Verified -> IntegrationAuthorized -> Integrated -> Reconciled`

Cancellation and failure are durable terminal outcomes from every non-terminal phase.

A prepared change is an immutable worker commit bound to its base head, tree, patch fingerprint, changed-path scope, implementation evidence, and worktree verification evidence. The primary repository remains byte-for-byte unchanged while the parent reviewer inspects this packet. Review acceptance is a durable receipt bound to the exact prepared commit and patch. A revision request invalidates all later receipts and preserves the isolated worktree for a bounded retry.

Independent verification runs from a fresh detached worktree at the reviewed commit, outside the implementation worker's authority. Its receipt is bound to the same commit, tree, changed paths, and verification evidence. Integration authorization exists only when accepted review and passing independent verification receipts agree on the exact prepared commit and the primary head still matches the transaction base. Any primary-head drift invalidates authorization and requires review and verification again.

Integration is idempotent and accepts only the authorized commit. Recovery detects an already-applied commit or equivalent tree, persists the missing integration receipt, reconciles the durable state, and only then removes isolated worktrees and branches.

The runtime emits distinct lifecycle events for prepared, reviewing, revision requested, review accepted, verifying, verified, authorized, integrated, reconciled, cancelled, and failed states. Frontends remain projections of this shared transaction rather than separate mutation authorities.

## Consequences

- Review rejection and verification failure cannot mutate the primary repository.
- A reviewed commit cannot be substituted without invalidating review, verification, and authorization.
- Duplicate integration attempts converge on one receipt and one repository result.
- Crash recovery can resume deterministically from every persisted phase.
- Direct, coordinated, daemon, desktop, GitHub-driven, and remote mutations share the same runtime transaction boundary.
- The legacy integrate-before-review compatibility fixture is removed only after the semantic ordering gate passes.
