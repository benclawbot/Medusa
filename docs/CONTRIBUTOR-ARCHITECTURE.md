# Contributor Architecture Map

This map connects the legacy product architecture in [`ARCHITECTURE.md`](ARCHITECTURE.md) to authoritative Rust crates and entrypoints. Architecture-v2 migration and certification are governed by [`architecture/INDEX.md`](architecture/INDEX.md) and [`architecture/baseline.json`](architecture/baseline.json). Legacy availability must not be interpreted as v2 certification.

## Runtime entrypoints

| Surface | Authoritative path | Responsibility |
|---|---|---|
| Shared production runtime | `medusa-runtime::RuntimeController -> run_prompt` | Owns shared runtime events, provider construction, cancellation, follow-ups, session continuity, and coordinated-mode selection. |
| Production agent engine | `medusa-agent::AgentEngine` | Executes one model-backed session under its runtime role policy. |
| Production multi-agent coordinator | `run_prompt -> multi_agent_coordinator::run_preflight` | Owns current bounded read-only teammate dispatch, durable leases, dependency evidence, cancellation, and repository-snapshot binding; it is called by production `run_prompt`. |
| Production mutating worker coordinator | `run_prompt -> mutating_worker_coordinator::run_implementation` | Owns the current implementer worktree, scope validation, worktree verification, commit preparation, integration, rollback, and cleanup; it is called by production `run_prompt`. V1 integrates before parent review and is quarantined for v2 replacement. |
| Terminal UI | `crates/medusa-tui` | Renders and drives the shared runtime interactively. |
| Desktop application | `apps/medusa-desktop` | React/Tauri projection over the shared runtime and daemon boundary. |
| Headless CLI | `crates/medusa-cli` | Starts scripted objectives, resume flows, maintenance commands, and explicit approval allowlists. |
| Daemon | `crates/medusa-daemon` | Hosts durable local and remote runtime sessions. |

The workspace metadata in the root `Cargo.toml` remains the machine-readable authority for the current production execution path. It describes v1 behavior, not the v2 target. The source-to-entrypoint proof is in [`PRODUCTION-EXECUTION-TRACE.md`](PRODUCTION-EXECUTION-TRACE.md).

## Plan

| Responsibility | Current ownership | V2 decision |
|---|---|---|
| Objective and goal state | `crates/medusa-goal`, `crates/medusa-world-model` | adapt behind foundation contracts |
| Context retrieval and turn assembly | `crates/medusa-context`, `crates/medusa-context-retrieval`, `crates/medusa-turn-assembly` | adapt behind versioned context/evidence contracts |
| Persisted session and plan state | `crates/medusa-agent`, `crates/medusa-memory`, `crates/medusa-session-continuity` | replace competing projections with one session/plan aggregate |
| Production task contracts and mutation classification | `crates/medusa-runtime` | replace with the v2 orchestration core |

## Execute safely

| Responsibility | Current status | Current ownership | V2 decision |
|---|---|---|---|
| Read-only teammate scheduling | shipped legacy behavior | `crates/medusa-runtime`, `crates/medusa-multi-agent-scheduler`, `crates/medusa-worker-leases` | replace task and worker authority |
| Parent/teammate evidence integration | shipped for planner and risk reviewer | `crates/medusa-runtime`, `crates/medusa-agent` | replace with versioned evidence and review receipts |
| Mutating worker isolation and integration | shipped legacy behavior | `crates/medusa-runtime`, `crates/medusa-workers` | preserve isolation; replace ordering so review precedes integration |
| Parent production review | read-only but after integration | `crates/medusa-runtime`, `crates/medusa-agent`, `crates/medusa-review-model` | quarantine current order; require independent prepared-change review |
| Transaction coordination | supporting contracts | `crates/medusa-transaction-coordinator`, `crates/medusa-agent` | replace with one mutation authority and durable receipts |
| Process containment | shipped, platform-limited | `crates/medusa-process-containment`, `crates/medusa-process-registry` | preserve and enforce the #653 unsafe/FFI boundary |
| Tool policy and control | shipped across agent paths | `crates/medusa-tool-policy`, `crates/medusa-tool-control`, `crates/medusa-agent` | adapt into capability permission and dispatch contracts |
| Repository verification gate | shipped but changed-path propagation is incomplete | `crates/medusa-agent`, `crates/medusa-runtime`, `crates/medusa-hardening` | replace with changed-path-aware verification receipts |
| Shared runtime events | shipped | `crates/medusa-protocol`, `crates/medusa-runtime` | preserve protocol intent; version command/event/evidence envelopes |

Current coordinated execution constructs separate planner and risk-reviewer sessions. A mutating objective creates one implementer worktree, runs an implementer `AgentEngine`, validates scope, calls targeted verification without explicit changed paths, prepares a commit, and integrates it. The read-only parent then reviews the already integrated result and owns the final repository verification gate. This order is a known v1 compatibility fixture, not a v2 invariant.

## Recover

| Responsibility | Current ownership | V2 decision |
|---|---|---|
| Worker leases and restart epochs | `crates/medusa-worker-leases`, `crates/medusa-agent` | adapt into the single worker aggregate |
| Worktree crash recovery and cleanup | `crates/medusa-runtime`, `crates/medusa-workers` | preserve proven mechanics behind v2 mutation contracts |
| Checkpoints and replay | `crates/medusa-execution-checkpoint`, `crates/medusa-execution-replay`, `crates/medusa-time-travel` | adapt behind versioned evidence/state contracts |
| Runtime supervision | `crates/medusa-runtime-supervisor`, `crates/medusa-daemon` | adapt into shared lifecycle state |
| Recovery coordination | `crates/medusa-recovery-coordinator` | adapt; make every recovery decision durable |
| Durable memory and learning | `crates/medusa-memory`, `crates/medusa-markdown-memory`, `crates/medusa-memory-writeback`, `crates/medusa-memory-consolidation`, `crates/medusa-improvement` | preserve verified outcomes and remove duplicate authority |

## Provider, integration, frontend, and distribution boundaries

| Boundary | Current ownership | V2 requirement |
|---|---|---|
| Provider routes and readiness | `crates/medusa-provider`, `crates/medusa-openai-realtime`, `crates/medusa-config` | exact capability/wire agreement, abortable requests, durable route health |
| GitHub OAuth and operations | `crates/medusa-github`, `crates/medusa-capabilities` | one typed, approval-gated, transport-neutral service |
| Browser sidecar | `crates/medusa-browser-client`, `crates/medusa-browserd` | no advertised action before dispatcher, permissions, and behavioral proof |
| Plugins and extensions | `crates/medusa-extensions` | versioned manifest, least privilege, isolation, lifecycle, durable result |
| Desktop | `apps/medusa-desktop`, `crates/medusa-daemon` | presentation over shared commands, events, evidence, and artifacts |
| Updates | `crates/medusa-update`, `crates/medusa-cli` | replace source compilation with #655 Ed25519-verified prebuilt artifacts |

## Persisted authority by concern

| Concern | Current authority | V2 target |
|---|---|---|
| Plans | session plan and runtime task contracts | one persisted plan aggregate |
| Execution | worker controller, team state, worktree state, and session evidence | one session/task/worker lifecycle |
| Verification | worktree checks and repository gate | changed-path-aware versioned receipt |
| Review | parent review after integration | independent accepted review receipt before integration |
| Reports | runtime/session reporting paths | pure projection from evidence |
| Learning | memory and improvement crates | provenance-bearing verified outcomes only |
| Recovery | checkpoint, replay, failure, supervisor, and recovery paths | durable recovery decisions over one authority graph |

## Change procedure

Changes to authority, mutable state, contracts, trust boundaries, dependency directions, entrypoints, capabilities, persistence, or lifecycle ordering must update the living index and machine-readable baseline. Pull requests must complete the architecture-impact declaration, identify migration consumers and a legacy deletion target, and add or update an ADR where the decision changes.

The executable gates are:

- `python scripts/check-product-architecture.py` for the current production trace;
- `python scripts/check-capability-evidence.py` for legacy availability evidence;
- `python scripts/check-architecture-index.py` for v2 inventory and governance;
- `python scripts/architecture-conformance.py --all --binary <medusa>` for real-entrypoint and known-failure conformance.
