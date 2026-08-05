# Production multi-agent consolidation

Issue #550 established one production authority for coordinated execution. The certified shipped path is:

`RuntimeController -> production_orchestrator -> multi_agent_coordinator -> mutating_worker_coordinator -> typed worktree verification -> dedicated durable parent reviewer -> independent verification -> authorization -> integration -> reconciliation -> canonical terminal persistence`.

## Retained authorities

- `RuntimeController` and its coordinators own task, worker, cancellation, recovery, review handoff, integration, reconciliation, and repository-verification lifecycle.
- `AgentEngine` owns bounded implementer, planner, and risk-reviewer model sessions with role-bound tools; it does not own parent mutation acceptance.
- The dedicated zero-tool parent reviewer owns typed review of the immutable prepared change and persists restart-safe `parent-review-session.json` evidence before authorization or integration.
- `MutationTransaction` is the authoritative durable mutation state machine. Independent verification, authorization, integration, reconciliation, and terminal completion consume its typed receipts.
- `WorkerExecutionController`, worker leases, the multi-agent scheduler, and `medusa-workers` retain durable scheduling and isolated worktree invariants.
- `medusa-agent::transaction` remains the atomic single-tool filesystem helper.
- `medusa-transaction-coordinator` remains an explicit capability implementation.

## Removed alternatives

The unused autonomous agent state machine, proposal/consensus/barrier transaction pipeline, uncalled lifecycle facade, isolated execution-orchestrator chain, conversational parent-review parser, generic `AgentSession` review adapter, integrate-before-review compatibility path, and their orphan-only crates were removed after production replacement proof. The serialized worker proposal DTO remains only as part of durable worker-completion evidence. No live `.medusa` coordinator, team, worker, checkpoint, replay, or recovery schema was deleted.

## Source layout

`medusa-runtime` compiles from ordinary Rust modules. Generated roots, build-time source rewriting, `OUT_DIR` module bindings, and `.inc` assembly are not part of the build. The capability-evidence manifest names `src/lib.rs` as the direct runtime integration authority, `parent_reviewer.rs` as the dedicated review authority, and `mutation_transaction_state.rs` as the durable mutation state authority.

## Validation contract

The consolidated workspace must remain formatted, compile with all targets and features, pass strict Clippy without warnings, and preserve the production runtime, dedicated-reviewer, transaction, agent, and worktree-worker test suites. Repository architecture, product-acceptance, safety/recovery, desktop, daemon, provider, multimodal, quickstart, and release workflows remain merge authority. The architecture checker rejects reintroduction of a generic parent `AgentEngine`, active migration freeze, post-integration review authority, or duplicate production execution state machine.
