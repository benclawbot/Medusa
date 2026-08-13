# Contributor Architecture Map

This map connects the product architecture in [`ARCHITECTURE.md`](ARCHITECTURE.md) to authoritative Rust crates and entrypoints. Architecture-v2 migration and certification are governed by [`architecture/INDEX.md`](architecture/INDEX.md) and [`architecture/baseline.json`](architecture/baseline.json). Availability claims must not exceed the certified production paths and executable evidence.

## Runtime entrypoints

| Surface | Authoritative path | Responsibility |
|---|---|---|
| Shared production runtime | `medusa-runtime::RuntimeController -> run_prompt` | Owns shared runtime events, provider construction, cancellation, follow-ups, session continuity, workspace selection, and coordinated-mode selection. |
| Production agent engine | `medusa-agent::AgentEngine` | Executes one model-backed session under its runtime role policy. It does not own team expansion or integration. |
| Production multi-agent coordinator | `run_prompt -> multi_agent_coordinator::run_preflight` | Owns bounded read-only teammate dispatch, durable leases, dependency evidence, cancellation, and workspace-snapshot binding; it is called by production `run_prompt`. |
| Production mutating worker coordinator | `run_prompt -> mutating_worker_coordinator::run_implementation` | Owns workspace-isolated implementation, scope validation, candidate verification/preparation, retry/recovery, and entry into the mutation transaction; it is called by production `run_prompt`. |
| Git parallel mutation planner | `parallel_mutation::decomposition_for` | Converts sufficiently safe exact-file Git scope into a conflict-aware `MutationDag`, otherwise returns single-implementer fallback. |
| Git aggregate authority | `parallel_mutation_batch::prepare_combined` | Accepts child evidence, establishes `IntegrationBarrier`, deterministically stages accepted children, validates aggregate scope, verifies the aggregate, and prepares the aggregate transaction. |
| Workspace mutation adapter | `workspace_worker_manager::WorkspaceWorkerManager` | Delegates Git workspaces to `medusa-workers` unchanged and supplies content-addressed isolation/integration for ordinary directories. |
| Workspace API | `medusa-runtime::workspace::Workspace` | Represents Git, directory, and explicit Medusa-owned ephemeral roots for programmatic callers. |
| Mutation transaction | `mutation_transaction_state` + dedicated `parent_reviewer` | Owns zero-tool parent review, independent verification, integration authorization, guarded integration, reconciliation, and durable receipts. |
| Terminal UI | `crates/medusa-tui` | Renders and drives the shared runtime interactively. |
| Desktop application | `apps/medusa-desktop` | React/Tauri projection over the shared runtime and daemon boundary. |
| Headless CLI | `crates/medusa-cli` | Starts scripted objectives, resume flows, maintenance commands, and explicit approval allowlists. `--repo` remains the compatibility name for the selected workspace root. |
| Daemon | `crates/medusa-daemon` | Hosts durable local and remote runtime sessions. |

The workspace metadata in root `Cargo.toml` is the machine-readable production execution summary. The source-to-entrypoint proof is [`PRODUCTION-EXECUTION-TRACE.md`](PRODUCTION-EXECUTION-TRACE.md); workspace behavior is documented in [`WORKSPACES.md`](WORKSPACES.md).

## Plan

| Responsibility | Current ownership |
|---|---|
| Objective and goal state | `crates/medusa-goal`, `crates/medusa-world-model` |
| Context retrieval and turn assembly | `crates/medusa-context`, `crates/medusa-context-retrieval`, `crates/medusa-turn-assembly` |
| Persisted session and plan state | `crates/medusa-agent`, `crates/medusa-memory`, `crates/medusa-session-continuity` |
| Production task contracts and mutation classification | `crates/medusa-runtime/src/production_orchestrator.rs` |
| Conflict-aware mutation resources and DAGs | `crates/medusa-multi-agent-scheduler`, `crates/medusa-runtime/src/parallel_mutation.rs` |

Planning may produce a broad parent implementation contract, but parallel mutation never hands that broad contract directly to children. Every Git child is derived into an exact narrowed ownership contract. Directory mutation deliberately remains single-implementer.

## Execute safely

| Responsibility | Shipped ownership | Invariant |
|---|---|---|
| Read-only teammate scheduling | `crates/medusa-runtime`, `crates/medusa-multi-agent-scheduler`, `crates/medusa-worker-leases` | Planner/risk workers cannot mutate. |
| Parent/teammate evidence integration | `crates/medusa-runtime`, `crates/medusa-agent` | Dependency evidence cannot redefine implementer tool authority. |
| Git worktree isolation | `crates/medusa-workers`, runtime coordinator | One worktree per implementer; child cannot integrate itself. |
| Git parallel implementation | `parallel_mutation.rs`, `parallel_mutation_batch.rs` | At most three safe children; exact resources; deterministic dependency order; aggregate verification before final transaction. |
| Directory mutation isolation | `workspace_worker_manager.rs` | One copied content-addressed candidate, symlink fail-closed, primary-drift rejection, rollback-protected authorized paths. |
| Parent production review | `parent_reviewer.rs`, `crates/medusa-review-model` | Dedicated reviewer is zero-tool and reviews an immutable candidate before integration. |
| Transaction coordination | `mutation_transaction_state.rs`, `mutation_transaction.rs` | Review, independent verification, authorization, integration, and reconciliation are separate durable transitions. |
| Process containment | `crates/medusa-process-containment`, `crates/medusa-process-registry` | Platform containment fails closed when required execution isolation is unavailable. |
| Tool policy/control | `crates/medusa-tool-policy`, `crates/medusa-tool-control`, `crates/medusa-agent` | Prompts are not an authorization boundary. |
| Verification authority | `crates/medusa-evidence`, `crates/medusa-agent`, `crates/medusa-runtime` | Verification binds to exact candidate identity and typed changed components. |
| Shared runtime events | `crates/medusa-protocol`, `crates/medusa-runtime` | Frontends present state; they do not create alternate execution authority. |

### Git multi-implementer lifecycle

```text
parent implementation contract
  -> typed mutation resources
  -> MutationDag::build
  -> conflict-free waves (<= 3 children total)
  -> one Git worktree + narrowed contract per child
  -> child scope validation + targeted verification
  -> child parent-review / independent-verification evidence
  -> IntegrationBarrier
  -> deterministic aggregate worktree staging
  -> aggregate scope validation + verification
  -> immutable aggregate transaction
  -> dedicated parent review
  -> independent verification
  -> authorization
  -> primary integration + reconciliation
  -> final workspace verification
```

Do not add a shortcut that cherry-picks child work into the primary repository before the aggregate transaction is reviewed and verified. Do not make `agent.parallel_workers` an implicit authorization for mutation DAG width; the mutation planner owns its separate safety bound.

### Directory / ephemeral mutation lifecycle

```text
workspace content fingerprint
  -> immutable baseline snapshot
  -> isolated copied worker root
  -> one implementer AgentEngine
  -> typed changed-component diff
  -> content-addressed immutable candidate
  -> detached candidate materialization
  -> dedicated parent review + independent verification
  -> primary fingerprint still equals baseline?
  -> authorized atomic path application with rollback copies
  -> resulting tree equals authorized candidate
  -> reconciliation + final workspace verification
```

Directory mutation excludes `.medusa`, `target`, and `node_modules` from candidate identity and fails closed on symlinks. If a future non-Git parallel backend is added, it must implement an aggregate barrier with equivalent review/recovery evidence rather than reusing unsafe shared-directory writes.

## Delegation authority

Current production parallelism is centrally scheduled. The following are **not** worker capabilities:

- spawning nested implementation workers;
- increasing the mutation-worker budget;
- widening allowed write paths;
- changing dependency/resource ownership;
- accepting sibling evidence as final verification;
- integrating into the primary workspace;
- bypassing parent review or independent verification.

Autonomous recursive delegation remains a separate future promotion boundary even if analysis-workspace or scheduler scaffolding exists elsewhere in the repository.

## Recover

| Responsibility | Current ownership |
|---|---|
| Worker leases and restart epochs | `crates/medusa-worker-leases`, `crates/medusa-agent` |
| Git worktree crash recovery/cleanup | `crates/medusa-runtime`, `crates/medusa-workers` |
| Directory baseline/candidate recovery | `workspace_worker_manager.rs`, `.medusa/executions/...` durable snapshot state |
| Mutation transaction reconciliation | `mutation_transaction_state.rs` |
| Checkpoints and replay | `crates/medusa-execution-checkpoint`, `crates/medusa-execution-replay`, `crates/medusa-time-travel` |
| Runtime supervision | `crates/medusa-runtime-supervisor`, `crates/medusa-daemon` |
| Recovery coordination | `crates/medusa-recovery-coordinator` |
| Durable memory and learning | memory/improvement crate families; only verified outcomes are positive learning |

Git recovery can prove prior integration through ancestry/tree identity. Directory recovery uses content-addressed candidate/tree identity and exact primary baseline checks. Both fail closed on stale state.

## Provider, integration, frontend, and distribution boundaries

| Boundary | Current ownership | Requirement |
|---|---|---|
| Provider routes/readiness | `crates/medusa-provider`, `crates/medusa-openai-realtime`, `crates/medusa-config` | Exact capability/wire agreement, abortable requests, durable route health. |
| GitHub OAuth/operations | `crates/medusa-github`, `crates/medusa-capabilities` | Typed, approval-gated service; not general workspace authority. |
| Browser sidecar | `crates/medusa-browser-client`, `crates/medusa-browserd` | No model-executable advertisement before dispatcher, permissions, behavioral proof. |
| Plugins/extensions | `crates/medusa-extensions` | Versioned manifest, least privilege, isolation, lifecycle, durable result. |
| Desktop | `apps/medusa-desktop`, `crates/medusa-daemon` | Presentation over shared commands/events/evidence/artifacts. |
| Updates | `crates/medusa-update`, `crates/medusa-cli` | Verified prebuilt release channel and health-checked replacement. |

## Persisted authority by concern

| Concern | Current authority |
|---|---|
| Plans | persisted plan/task graph, typed mutation DAG when active |
| Execution | worker controller, team state, isolated worktree/snapshot state, session evidence |
| Verification | typed changed-component VerificationPlan/VerificationReceipt and final workspace gate |
| Review | dedicated zero-tool accepted/revision/reject receipt before integration |
| Authorization | mutation transaction after independent verification |
| Integration | workspace backend invoked only through authorized transaction state |
| Reports | runtime/session projection from durable evidence |
| Learning | provenance-bearing verified outcomes only |
| Recovery | checkpoint/replay/failure/supervisor plus transaction and candidate identity evidence |

Schema names such as `prepared_commit` and `prepared_tree` are durability contracts, not proof that every workspace uses Git. Directory mode stores content-addressed snapshot/tree identifiers in those fields.

## Change procedure

Changes to authority, mutable state, contracts, trust boundaries, dependency directions, entrypoints, capabilities, persistence, workspace backends, or lifecycle ordering must update the living index/machine-readable baseline where applicable, public architecture, capability evidence, and executable tests.

The executable gates include:

- `python scripts/check-product-architecture.py` for the production trace and workspace claims;
- `python scripts/check-capability-evidence.py` for availability evidence;
- `python scripts/check-architecture-index.py` for v2 inventory/governance;
- `python scripts/architecture-conformance.py --all --binary <medusa>` for real-entrypoint conformance;
- **Parallel Mutation Certification** for the conflict-aware Git implementation DAG;
- **Workspace Backend Certification** for cross-platform non-Git workspace isolation and architecture drift.
