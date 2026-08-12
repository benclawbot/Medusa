# ADR-0001: Rebuild Medusa around one versioned core

- Status: Accepted
- Date: 2026-08-02
- Decision owners: architecture and runtime maintainers
- Program: #645
- Baseline: #646

## Context

Medusa accumulated substantial infrastructure and many crate-level abstractions, but production entrypoints, capability claims, mutable state, provider readiness, review ordering, verification inputs, and frontend behavior are not governed by one coherent contract. Several paths are advertised more strongly than their dispatcher or runtime evidence supports. Adding more features would multiply incompatible authorities and increase the cost of repair.

A greenfield rewrite would discard proven containment, persistence, GitHub, release, recovery, and cross-platform work. Continuing incremental feature work on the current authority graph would preserve the architectural contradictions.

## Decision

Medusa will be rebuilt in place through architecture-v2 phases #646–#652.

1. Freeze major feature expansion during the migration. Security/data-loss work, architecture-v2 enablement, #653, and #655 are allowed exceptions.
2. Preserve proven infrastructure where its contract and evidence remain valid; adapt it behind versioned v2 boundaries.
3. Replace competing orchestration, capability, review, verification, provider-readiness, and mutation authorities with one shared core.
4. Quarantine advertised or structural capabilities that lack a certified production dispatcher, permission model, behavioral proof, and durable lifecycle.
5. Drive every frontend through the same versioned command, event, evidence, artifact, provider, and capability contracts.
6. Require independent review of a prepared change before primary-tree integration.
7. Treat changed paths as mandatory verification and evidence input throughout the execution lifecycle.
8. Migrate one authority and its consumers at a time, with compatibility fixtures and rollback evidence.
9. Delete superseded v1 code, state, documentation, and workflows after each migration slice is proven. Compatibility adapters are temporary and must have deletion targets.

## Authority

- Current behavior: production code and executable tests on `main`.
- Legacy availability claims: `docs/CAPABILITY-CLAIMS.json` and `docs/CAPABILITY-EVIDENCE.md`.
- V2 certification, migration ownership, trust boundaries, and deletion targets: `docs/architecture/baseline.json`.
- Human navigation and invariants: `docs/architecture/INDEX.md`.

Legacy `production` is not equivalent to v2 `certified-production`.

## Consequences

Positive consequences:

- one traceable owner for each mutable concern;
- capability advertising becomes generated from certified dispatch evidence;
- provider and frontend behavior can no longer silently diverge;
- known failures remain reproducible while being explicitly rejected as target behavior;
- every compatibility layer has an accountable removal path;
- Linux, macOS, and Windows share the same baseline checks.

Costs and risks:

- feature velocity is intentionally reduced during migration;
- some currently visible capabilities will be downgraded or hidden;
- temporary adapters are required while persistent state and entrypoints move;
- the index and baseline become merge requirements and require active maintenance.

## Alternatives rejected

### Continue feature-by-feature repair

Rejected because the defects cross task state, mutation ordering, capability dispatch, providers, frontends, and persistence. Local fixes would continue to add competing authority.

### Full rewrite in a new repository

Rejected because it would abandon proven containment, recovery, persistence, release, and cross-platform evidence and would create a long period with two products and no safe migration authority.

### Documentation-only architecture map

Rejected because stale architecture is the current failure mode. The index must be backed by a machine-readable inventory, adversarial checks, black-box fixtures, pull-request policy, CODEOWNERS, and platform CI.

## Follow-up

- #647 defines foundation contracts.
- #648 establishes the single orchestration and state core.
- #649 establishes capability lifecycle, permissions, and dispatch.
- #650 establishes provider, OAuth, route health, and readiness authority.
- #651 migrates all frontends to the shared core.
- #652 migrates state and deletes v1.
- #653 scopes and audits unsafe/FFI.
- #655 replaces source-build updates with free Ed25519-verified prebuilt releases.
