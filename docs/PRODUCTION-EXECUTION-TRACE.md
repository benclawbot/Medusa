# Production execution trace

This document is the source-to-entrypoint authority for Medusa's shipped execution model.

## Authoritative model

Medusa production execution uses **bounded teammates, worktree-isolated mutation, a read-only parent lead, and repository-gated completion**.

```text
CLI / TUI / desktop / daemon consumer
  -> medusa_runtime::RuntimeController
  -> worker_loop_with_state
  -> run_prompt
  -> production_orchestrator::plan
       -> classifies whether repository mutation is required
  -> multi_agent_coordinator::run_preflight
       -> durable task leases and team state
       -> planner AgentEngine (read-only)
       -> risk-reviewer AgentEngine (read-only)
       -> validated dependency evidence
  -> mutating_worker_coordinator::run_implementation [mutating objectives only]
       -> execution-specific Git branch and worktree
       -> implementer AgentEngine (runtime-enforced implementer policy)
       -> changed-path scope validation
       -> targeted verification inside the worktree
       -> deterministic commit preparation
       -> guarded cherry-pick with overlap rejection and rollback
       -> durable integration receipt and cleanup
  -> parent AgentEngine (read-only reviewer)
  -> multi_agent_coordinator::verify_repository
  -> persisted outcome and AgentSession state
```

`RuntimeController` remains the shared frontend-neutral lifecycle boundary. Each `AgentEngine` executes exactly one session. The read-only coordinator owns dependency evidence; the mutating coordinator owns worktree mutation and integration; the parent owns review, user-facing reporting, and the final repository gate.

## Dispatch boundary

Coordinated prompts create independent first-wave repository-analysis and risk-review tasks. Explicit mutation language adds the implementer/review/verify task chain. Long analytical objectives retain coordinated read-only research but do not create a mutating worktree.

The current production mutation slice dispatches exactly one implementer contract. Its branch name is execution-specific and its worker identity and lease epoch are durable. Dynamic multi-implementer decomposition remains a later promotion boundary, although the underlying `WorkerManager` already rejects overlapping paths and rolls back a failed integration batch.

## Worktree isolation and integration

The implementer receives a role-bound mutating policy only inside its dedicated Git worktree. Per-session `.medusa` files are removed from the candidate patch, tracked changes are compared with the contract's allowed write scope, and verification must not mutate the candidate path set. The coordinator then squashes the work onto one deterministic commit and integrates it into a clean primary repository.

A conflict aborts the cherry-pick and resets the repository to the exact pre-integration HEAD. Successful cleanup removes the worktree and temporary branch while retaining the durable receipt.

## Durable state and restart semantics

Coordinator state is stored under `.medusa/executions/<execution-id>`. Durable records include team membership, mailboxes, task leases and epochs, repository and context fingerprints, worker session identifiers, changed paths, verification evidence, prepared commits, and integration receipts.

A crash before the first state write may reopen an existing worktree only when its branch and base still exactly match the primary HEAD. A crash after integration but before receipt persistence is detected through commit ancestry or exact tree identity. Interrupted leases are requeued with a higher epoch. Stale or mismatched state fails closed.

## Client entrypoints

The TUI, desktop bridge, CLI, and daemon all call the same `RuntimeController`. They render shared runtime events and do not maintain an independent scheduler, worktree manager, or completion model.

## Completion authority

Teammate output and an integrated commit are evidence, not completion. The parent is read-only during coordinated execution and cannot bypass worktree isolation with direct writes. A coordinated objective may report completed only after the primary repository verification gate succeeds. Ordinary turn boundaries are not treated as verified completion.

## Remaining promotion boundary

Autonomous nested delegation, model-driven team expansion, consensus voting, commit barriers, and distributed multi-worker transaction coordination require separate production evidence. Their workspace crates are not the current integration authority.
