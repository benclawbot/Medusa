# Production execution trace

This document is the source-to-entrypoint authority for Medusa's shipped execution model.

## Authoritative model

Medusa production execution uses **bounded teammates, workspace-isolated mutation, a dedicated zero-tool parent reviewer, independent verification, and workspace-gated completion**.

```text
CLI / TUI / desktop / daemon consumer
  -> medusa_runtime::RuntimeController
  -> worker_loop_with_state
  -> run_prompt
  -> production_orchestrator::plan
       -> classifies whether workspace mutation is required
  -> multi_agent_coordinator::run_preflight
       -> durable task leases and team state
       -> planner AgentEngine (read-only)
       -> risk-reviewer AgentEngine (read-only)
       -> validated dependency evidence
  -> mutating_worker_coordinator::run_implementation [mutating objectives only]
       -> workspace mutation revision
       -> parallel_mutation::decomposition_for
            -> Git + safe typed decomposition: conflict-aware DAG, <= 3 children
            -> otherwise: single implementer
       -> Git child: execution-specific branch/worktree
       -> directory child: isolated content-addressed snapshot workspace
       -> implementer AgentEngine (runtime-enforced implementer policy)
       -> changed-component scope validation
       -> targeted verification inside isolation
       -> immutable candidate preparation
       -> [parallel Git] child review/verification barrier + deterministic aggregate staging
       -> dedicated zero-tool parent review
       -> independent immutable-candidate verification
       -> integration authorization
       -> guarded integration + reconciliation
  -> parent AgentEngine (read-only lead/reporting role)
  -> primary workspace verification gate
  -> persisted outcome and AgentSession state
```

`RuntimeController` remains the shared frontend-neutral lifecycle boundary. Each `AgentEngine` executes exactly one session. The read-only coordinator owns dependency evidence; the mutation coordinator owns isolation and candidate preparation; the mutation transaction owns review/verification/authorization/integration state; the parent owns user-facing reporting and the final workspace gate.

## Dispatch boundary

Coordinated prompts create independent first-wave analysis/planning and risk-review tasks. Explicit mutation language adds the implementation/review/verify chain. Long analytical, documentation, and supplied-source research objectives retain coordinated read-only teammates without creating a mutating worker when no workspace mutation is required.

Git mutation may decompose into multiple implementation children only when the typed scope contains at least two exact files, risk is not high, confidence meets the production threshold, the task count remains within the three-mutator bound, and the mutation-resource graph is conflict-safe. Manifests, lockfiles, migrations, snapshots, generated outputs, dependencies, and exact path ownership participate in conflict analysis. Unsafe decomposition falls back to one worktree implementer rather than broadening authority.

Directory and ephemeral workspaces use one content-addressed isolated implementer. Non-Git parallel mutation is not advertised because the certified deterministic aggregate barrier currently uses Git worktree staging.

## Git parallel isolation and deterministic aggregation

Each Git child receives an exact narrowed task contract and its own worktree rooted at the immutable plan revision. A child cannot see sibling uncommitted state, cannot widen its own write scope, and cannot integrate itself. Changed components must remain inside the child contract and targeted verification must pass.

Accepted children independently traverse the parent-review and independent-verification authority far enough to establish their immutable evidence. `IntegrationBarrier` then binds accepted task evidence to the original DAG, upstream candidate fingerprints, and deterministic task order. A separate aggregate worktree composes children in that order, validates the aggregate scope, runs aggregate verification, and produces one immutable aggregate transaction. Only that aggregate can proceed through final review and primary integration.

The cross-platform Parallel Mutation Certification workflow exercises DAG behavior, production runtime wiring, deterministic integration, rollback/cleanup, fallback/scope invalidation, and performance evidence.

## Directory workspace isolation and integration

For an ordinary non-Git directory, the workspace worker backend computes a deterministic manifest/tree fingerprint excluding Medusa runtime state and common generated dependency/build trees. It copies that bounded revision into a dedicated worker directory and stores the baseline under durable execution state.

After implementation, Medusa removes runtime residue, compares the baseline and worker manifests into typed `ChangedComponent` evidence, validates contract scope, verifies the candidate, and stores the accepted worker tree as an immutable `dir-<sha256>` content-addressed snapshot. Symbolic links fail closed for directory mutation.

The durable mutation transaction retains its schema-stable `prepared_commit` and `prepared_tree` fields; in this backend they contain snapshot/tree identifiers rather than Git object IDs. The zero-tool parent reviewer receives a bounded text-or-digest patch. Independent verification materializes the immutable snapshot into a separate directory. Integration is authorized only if the primary directory still has the exact baseline fingerprint. Accepted paths are replaced atomically with rollback copies, and the resulting primary tree must equal the authorized snapshot before reconciliation succeeds.

## Durable state and restart semantics

Coordinator state is stored under `.medusa/executions/<execution-id>`. Durable records include team membership, mailboxes, task leases and epochs, workspace/context fingerprints, worker session identifiers, changed components, verification evidence, immutable candidates, parent-review receipts, independent-verification receipts, integration authorization, and integration/reconciliation receipts.

A Git crash before the first state write may reopen an existing worktree only when its branch and base still exactly match the primary HEAD. A Git crash after integration but before receipt persistence is detected through commit ancestry or exact tree identity. Directory workers bind to a content baseline; prepared snapshots are immutable and primary drift fails closed before integration. Interrupted leases are requeued with a higher epoch. Stale or mismatched state fails closed.

## Client entrypoints

The TUI, desktop bridge, CLI, and daemon all call the same `RuntimeController`. Programmatic callers may additionally construct `medusa_runtime::workspace::Workspace` values for Git, directory, or Medusa-owned ephemeral roots. Frontends do not maintain an independent scheduler, workspace manager, mutation transaction, or completion model.

## Completion authority

Teammate output and an immutable candidate are evidence, not completion. The dedicated parent reviewer is read-only and zero-tool. Review acceptance is not integration authority; independent verification and authorization remain separate durable transitions. A coordinated objective may report completed only after authorized integration/reconciliation where mutation occurred and the primary workspace verification gate succeeds. Ordinary turn boundaries are not treated as verified completion.

## Remaining promotion boundary

Autonomous nested delegation, model-driven unconstrained team expansion, consensus voting, distributed multi-host/multi-process mutation transactions, and non-Git parallel mutation require separate production evidence. Browser actions also remain quarantined from model-executable surfaces until their dispatcher and permission evidence are certified.
