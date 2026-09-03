# Medusa Glossary

One-paragraph index of the terms in the `medusa-*` namespace. New contributors should
read this once before opening a design discussion.

## Product model

- **Plan / Execute Safely / Recover** — Medusa's product model. Plan: an objective and
  workspace context become explicit task contracts and a reviewable plan. Execute Safely:
  read-only teammates scout; mutation runs in isolated worktrees; review, verification,
  authorization, and integration are separate runtime authorities. Recover: durable
  `.medusa/` authority means interruption, cancellation, or crash is never rewritten as
  success. See [`docs/ARCHITECTURE.md`](ARCHITECTURE.md).

## Runtime

- **Task contract** — typed objective + scope + verification surface handed to a worker.
  Workers cannot expand contracts; only the root coordinator creates them. See
  [`docs/ARCHITECTURE.md` § Orchestration](ARCHITECTURE.md#orchestration-and-parentsubagent-responsibility).
- **MutationDag** — conflict-aware parallel mutation graph. Built only for exact,
  sufficiently confident, non-high-risk scopes within the bounded three-mutator budget.
  Specialized resources cover manifests, lockfiles, migrations, snapshots, and generated
  outputs. See [`docs/CAPABILITY-EVIDENCE.md` § Multi-agent](CAPABILITY-EVIDENCE.md).
- **IntegrationBarrier** — staging point where accepted parallel children are aggregated
  in deterministic dependency order before final review. See
  [`medusa-multi-agent-scheduler::mutation_dag`](../crates/medusa-multi-agent-scheduler/src/mutation_dag.rs).
- **ImmutableCandidate** — content-addressed workspace candidate persisted by a worker.
  Independent verification reads from this; only authorized integration mutates the
  primary workspace.
- **MutationTransaction** — irreversible workspace write prepared after verification. The
  final unit of authorized mutation.
- **WorkerLease / epoch** — durable implementer identity. A new epoch on lease recovery;
  interrupted leases never silently succeed. See
  [`crates/medusa-worker-leases`](../crates/medusa-worker-leases/).
- **Workspace verification gate** — authoritative completion check. Configured per
  workspace via `verify.sh`, `verify.ps1`, or a recognized project verification path. See
  [`docs/WORKSPACES.md`](WORKSPACES.md).

## Provider / cross-model context

- **ProviderContinuationState** — provider-native opaque continuation bytes. Exact-bound
  by provider/protocol/route/model/session by default. Redacted from debug and
  serialization. Has no provider-neutral transcript or prompt rendering path. Unsupported
  or incompatible continuation fails closed. See
  [`docs/ARCHITECTURE.md` § Provider role routing](ARCHITECTURE.md#provider-role-routing-and-reasoning-exchange).
- **ReasoningHandoffV1** — provider-neutral cross-model context contract. Contains
  bounded, visible decision state, evidence references, verification receipts, risks,
  and next actions with trust and sensitivity metadata. Transfer policy: `none`,
  `evidence_only`, `decisions_and_evidence`, `structured`, or conservative `auto`.
  Independent review omits source decisions.

## Evolution

- **Transactional component-runtime contract** — safe harness evolution pattern shipped
  by `medusa-runtime`. Stable component generations, scoped host context, resource
  ownership, reversible effect journals, declarative dependencies, committed-vs-target
  provider views, ordered retirement, versioned desired state with compare-and-swap
  updates, health-validated replacement, containment-bound capabilities, validated
  self-modification proposals, explicit external-commit semantics, deterministic
  fault/invariant checks. See
  [`docs/SCHEMA_HARNESS_FOUNDATION.md`](SCHEMA_HARNESS_FOUNDATION.md).

## Durability

- **Reversible-effect journal** — Medusa's record of effects it can still roll back
  (pre-integration state, candidate revisions). See
  [`docs/durable-journal-policy.md`](durable-journal-policy.md).
- **External commit** — anything that has already hit the filesystem or Git remote and
  cannot be rolled back by Medusa alone. Recovery stops at the last authoritative state.

## Authority

- **`.medusa/`** — durable authority directory at the workspace root. Holds sessions,
  plans, events, approvals, worker leases, immutable candidates, delegation contracts,
  agent scopes, effective model-request manifests, transactions, verification, and
  recovery state. The runtime cannot rewrite interruption as success here.
