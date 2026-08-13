# Mutating Worktree Integration

This document describes the **Git workspace backend**. Medusa can also mutate ordinary directories through content-addressed isolated snapshots; see [Workspace modes](WORKSPACES.md).

Coordinated Git implementation must not edit the user's primary working tree directly.

## Production boundary

- The coordinator creates a dedicated Git branch and worktree for each mutating implementer.
- Each implementer runs an independent `AgentEngine` session with the runtime-enforced implementer tool policy.
- Every worktree is created from the repository revision that the execution plan was built against.
- Changed components are collected from Git and must remain within the task contract's allowed write scope.
- Verification runs inside the isolated worktree before any candidate is accepted.
- Successful work is committed with deterministic Medusa author identity and persisted as durable evidence.
- The coordinator rejects unsafe mutation-resource conflicts and overlapping ownership before concurrent execution/integration.
- Accepted parallel children are composed through a deterministic integration barrier into a separate aggregate worktree; the primary repository is still unchanged at that point.
- The aggregate candidate is scope-checked and verified before it enters final parent review.
- The dedicated parent reviewer has zero tools. Accepted review is followed by independent immutable-candidate verification and explicit integration authorization.
- Authorized commits are integrated only by the runtime transaction authority. A conflict or primary-HEAD drift fails closed; rollback restores the pre-integration HEAD.
- Temporary worktrees and branches are removed after acceptance or rejection; durable evidence remains under `.medusa`.
- The user-facing parent session is read-only during coordinated execution and cannot bypass worktree isolation with direct file writes.
- Final primary repository verification remains the coordinated completion gate.

## Bounded parallel implementation

The production Git path supports conflict-aware parallel implementation rather than an unrestricted agent swarm. A typed implementation scope may decompose into at most three child mutators when:

- at least two exact file ownership scopes are known;
- risk is below the high-risk boundary;
- decomposition confidence is at least the production threshold;
- no child requires ambiguous repository- or directory-wide ownership;
- the mutation DAG can order or separate conflicting resources safely.

The mutation-resource model includes exact paths plus specialized resources for manifests, lockfiles, migrations, snapshots, and generated outputs. Dependencies and resource conflicts create serialization edges; only conflict-free tasks share a wave.

Every child gets a narrowed contract and independent worktree. A child cannot recursively spawn another implementer, integrate itself, or alter a sibling's scope. Each child candidate receives scope validation, targeted verification, parent review, and independent verification evidence before the `IntegrationBarrier` can accept it. The aggregate worktree then deterministically stages accepted children in dependency order, validates the union scope, and runs aggregate verification before final review/integration.

When these safety conditions are not met, Medusa automatically uses one isolated implementer. High-risk mutation is always single-implementer.

## Directory workspace distinction

Non-Git directories deliberately do **not** reuse Git-specific worktree/cherry-pick semantics. They use one isolated content-addressed snapshot implementer with baseline fingerprinting, immutable snapshot preparation, primary-drift detection, independent materialization, rollback-protected path application, and resulting-tree verification. Parallel non-Git mutation remains withheld until a separately certified aggregate transaction backend exists.

## Acceptance proof

The Git implementation must prove:

1. isolated workers cannot see or overwrite each other's uncommitted changes;
2. out-of-scope changes are rejected before acceptance;
3. resource/path conflicts produce deterministic serialization or rejection;
4. accepted children are composed only through the durable integration barrier;
5. aggregate scope and verification are checked before primary integration;
6. integration conflict rolls back to the exact pre-integration HEAD;
7. cancellation and worker failure preserve evidence and clean temporary resources;
8. successful integration removes worktrees and branches without deleting durable receipts;
9. direct single-agent mode retains its existing working-tree behavior;
10. autonomous nested delegation never becomes an implicit mutation authority.

The cross-platform **Parallel Mutation Certification** workflow is the dedicated production proof for the multi-implementer Git path.
