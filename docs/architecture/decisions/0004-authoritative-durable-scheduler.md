# ADR 0004: Authoritative durable planner and scheduler

- Status: Accepted
- Date: 2026-08-02
- Issue: #648

## Context

Production orchestration currently combines a heuristic prompt classifier, inferred write paths, a static schedule, separate durable worker controllers, model-authored plan presentation, and frontend-local worker projections. This permits displayed state and mutation authority to diverge from the accepted execution graph. In particular, unresolved mutation scope currently falls back to repository-wide authority.

## Decision

Medusa will use one typed planning result and one persisted execution ledger as the authority for coordinated execution.

The planning result records intent, requested outcomes, affected components, requested and effective repository scope, risk, confidence, required capabilities, execution strategy, and the accepted adaptive task graph. Unknown or invalid write scope grants no write authority and produces a read-only strategy.

Every task has a supported task kind, explicit dependencies, capabilities, repository scope, context fingerprint, cancellation authority, retry policy, and durable terminal evidence. Review and verification are executable tasks or gates in the same graph. Worker and plan presentation are projections of persisted scheduler records only.

Runtime coordinators may provide specialized execution mechanisms, such as isolated worktrees, but they may not create independent task authority or display workers absent from the accepted graph. Cancellation, restart recovery, revision loops, and retries update the same ledger deterministically.

Execution ledgers are namespaced by durable session identity and accepted plan fingerprint. Identical plans in unrelated sessions therefore cannot share task state, recovery decisions, cancellation state, or terminal evidence.

## Consequences

- Changing the accepted DAG changes dispatch order and executable work.
- Unsupported task kinds fail before any frontend projection.
- Ambiguous mutation requests remain read-only until scope is resolved.
- CLI, TUI, daemon, desktop, and replay consumers can reconstruct task and worker state from the same persisted record.
- Legacy keyword classification, repository-wide fallback scope, and model-authored coordinated plan authority are removed after migration.
