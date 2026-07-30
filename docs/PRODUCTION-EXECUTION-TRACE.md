# Production execution trace

This document is the source-to-entrypoint authority for Medusa's shipped execution model.

## Authoritative model

Medusa production execution uses **bounded read-only teammates with parent-owned mutation and completion**.

```text
CLI / TUI / desktop / daemon consumer
  -> medusa_runtime::RuntimeController
  -> worker_loop_with_state
  -> run_prompt
  -> production_orchestrator::plan
  -> multi_agent_coordinator::run_preflight
       -> durable task leases and team state
       -> planner AgentEngine (read-only)
       -> risk-reviewer AgentEngine (read-only)
       -> validated durable evidence
  -> parent AgentEngine::step_with_observer_and_context
  -> multi_agent_coordinator::verify_repository
  -> persisted outcome and AgentSession state
```

`RuntimeController` remains the shared frontend-neutral lifecycle boundary. The coordinator owns bounded teammate dispatch, durable leases, team state, evidence validation, cancellation, and the final repository gate. Each `AgentEngine` still executes exactly one agent session. The parent remains the sole mutation and integration authority in this production slice.

## Dispatch boundary

Coordinated prompts create two independent first-wave tasks: repository analysis and risk review. They run concurrently in separate read-only sessions with runtime-enforced tool policy. Their evidence is bound to both the execution plan and a deterministic repository-content fingerprint, so stale evidence cannot be reused after repository changes.

Mutating teammate dispatch is not part of this slice. Worktree isolation, guarded commit integration, overlap handling, and rollback must be promoted separately before implementer agents may write.

## Durable state and restart semantics

Coordinator state is stored under `.medusa/executions/<execution-id>`. The execution identifier combines the plan fingerprint and repository fingerprint. Durable records include team membership, lifecycle, mailboxes, task leases and epochs, worker completion state, session identifiers, context fingerprints, and validated evidence. Completed evidence can be reused only when both plan and repository fingerprints match.

Cancellation is shared with the parent runtime and checked before dispatch and during each worker turn. Worker failures are recorded through the lease controller and team lifecycle before the coordinated turn fails closed.

## Client entrypoints

The TUI, desktop bridge, CLI, and daemon all call the same `RuntimeController`. They render shared runtime events and do not maintain an independent scheduler or completion model.

## Completion authority

Teammate output is evidence, not completion. The parent reviews and uses that evidence, performs any mutation, and remains accountable for the resulting repository. A coordinated objective may report completed only after the repository verification gate succeeds. Ordinary turn boundaries are not treated as verified completion.

## Remaining promotion boundary

Mutating teammates, nested delegation, worktree commit integration, conflict resolution, and autonomous team steering require separate production evidence. Workspace crates implementing those concepts remain non-production until explicitly reachable from the coordinator and represented in the capability ledger.
