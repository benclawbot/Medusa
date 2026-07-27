# Production execution trace

This document is the source-to-entrypoint authority for Medusa's shipped execution model.

## Authoritative model

Medusa production execution is **single-agent**.

The only shipped interactive execution path is:

```text
CLI / TUI / desktop / daemon consumer
  -> medusa_runtime::RuntimeController
  -> worker_loop_with_state
  -> run_prompt
  -> medusa_agent::AgentEngine::step_with_observer_and_context
  -> repository verification and persisted AgentSession state
```

`RuntimeController` is the shared frontend-neutral boundary. `run_prompt` constructs exactly one `AgentEngine` for a configured provider and advances one persisted `AgentSession`. It does not create worker engines, acquire worker leases, dispatch scheduler assignments, invoke consensus, or integrate subagent results.

## Orchestration planning boundary

`medusa_runtime::orchestration_planning` contains decomposition, task-contract, and schedule metadata retained for future orchestration work. It is not a production entrypoint and is not called by `run_prompt`.

The historical source file remains named `production_orchestrator.rs` temporarily to keep this correction small and reviewable, but its public export is intentionally removed. New consumers must use the explicitly non-production `orchestration_planning` module name.

The following crates are implementation scaffolding unless a capability record explicitly promotes them with complete evidence:

- `medusa-multi-agent-scheduler`
- `medusa-workers`
- `medusa-worker-leases`
- `medusa-worker-read-set`
- `medusa-worker-transaction`
- `medusa-commit-barrier`
- `medusa-consensus`
- `medusa-conflict-resolution`
- `medusa-transaction-coordinator`

Their presence in the workspace does not make them reachable from production execution.

## Client entrypoints

The TUI, desktop bridge, CLI, and daemon must all construct or call the shared `RuntimeController`. They may present runtime events differently, but they must not maintain an independent execution engine or silently activate non-production orchestration.

## Persisted session semantics

Persisted production sessions describe one `AgentEngine` and one authoritative `AgentSession`. Planning contracts or schedule metadata must never be rendered as evidence that workers, subagents, delegation, parallel waves, review agents, or verifier agents actually ran.

Historical records containing planning labels are interpreted as planning metadata unless they include a future versioned execution record from a promoted production capability.

## Promotion rule

Worker or subagent execution may become production only after all of the following are true:

1. A production entrypoint invokes it explicitly.
2. Capability metadata marks it `production` with an owner and activation state.
3. Containment, approvals, recovery, verification, observability, and audit semantics are defined.
4. Cross-platform product acceptance proves the behavior.
5. Persisted schemas distinguish planned work from actually dispatched and completed work.
6. Architecture, contributor documentation, UI wording, and examples are updated in the same change.

Until then, repository guardrails must fail when production metadata or public wording implies worker or subagent dispatch.
