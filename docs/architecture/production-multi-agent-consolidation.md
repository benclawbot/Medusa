# Production multi-agent consolidation

Issue #550 established one production authority for coordinated execution. The shipped path is:

`RuntimeController -> production_orchestrator -> multi_agent_coordinator -> mutating_worker_coordinator -> read-only parent AgentEngine`.

## Retained authorities

- `RuntimeController` and its coordinators own task, worker, cancellation, recovery, integration, and repository-verification lifecycle.
- `AgentEngine` owns one bounded model session and role-bound tool execution.
- `WorkerExecutionController`, worker leases, the multi-agent scheduler, and `medusa-workers` retain durable scheduling and isolated worktree invariants.
- `medusa-agent::transaction` remains the atomic single-tool filesystem helper.
- `medusa-transaction-coordinator` remains an explicit capability implementation.

## Removed alternatives

The unused autonomous agent state machine, proposal/consensus/barrier transaction pipeline, uncalled lifecycle facade, isolated execution-orchestrator chain, and their orphan-only crates were removed after production replacement proof. The serialized worker proposal DTO remains only as part of durable worker-completion evidence. No live `.medusa` coordinator, team, worker, checkpoint, replay, or recovery schema was deleted.

## Source layout

`medusa-runtime` now compiles from ordinary Rust modules. Generated roots, build-time source rewriting, `OUT_DIR` module bindings, and `.inc` assembly are no longer part of the build.

## Validation contract

The consolidated workspace must remain formatted, compile with all targets and features, pass strict Clippy without warnings, and preserve the production runtime, agent, and worktree-worker test suites. Repository architecture, product-acceptance, safety-proof, desktop, daemon, provider, multimodal, quickstart, and release workflows remain the merge authority.
