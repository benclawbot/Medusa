# Contributor Architecture Map

This map connects the product architecture in [`ARCHITECTURE.md`](ARCHITECTURE.md) to authoritative Rust crates and entrypoints. Contributors use the crate graph to find ownership, while users reason about **Plan, Execute Safely, Recover**.

## Runtime entrypoints

| Surface | Authoritative path | Responsibility |
|---|---|---|
| Shared production runtime | `medusa-runtime::RuntimeController -> run_prompt` | Owns shared runtime events, provider construction, cancellation, follow-ups, session continuity, and coordinated-mode selection. |
| Production agent engine | `medusa-agent::AgentEngine` | Executes one model-backed session under its runtime role policy. |
| Production multi-agent coordinator | `run_prompt -> multi_agent_coordinator::run_preflight` | Owns bounded read-only teammate dispatch, durable leases, team lifecycle, dependency evidence, cancellation, and repository-snapshot binding. |
| Production mutating worker coordinator | `run_prompt -> mutating_worker_coordinator::run_implementation` | Owns implementer worktree creation and recovery, scope validation, worktree verification, deterministic commit preparation, guarded integration, rollback, and cleanup. |
| Terminal UI | `crates/medusa-tui` | Renders and drives the shared runtime interactively. |
| Desktop application | `apps/medusa-desktop` | React/Tauri frontend over the shared runtime and daemon boundary. |
| Headless CLI | `crates/medusa-cli` | Starts scripted objectives, resume flows, maintenance commands, and explicit approval allowlists. |

The workspace metadata in the root `Cargo.toml` is the machine-readable authority for the production execution model, entrypoint, worktree mutation boundary, read-only parent role, and verification gate. The complete trace is in [`PRODUCTION-EXECUTION-TRACE.md`](PRODUCTION-EXECUTION-TRACE.md).

## Plan

| Responsibility | Primary ownership | Supporting ownership |
|---|---|---|
| Objective and goal state | `crates/medusa-goal`, `crates/medusa-world-model` | `crates/medusa-context`, `crates/medusa-context-retrieval` |
| Turn and prompt assembly | `crates/medusa-turn-assembly`, `crates/medusa-agent` | `crates/medusa-prompt-cache`, `crates/medusa-provider` |
| Persisted session and plan state | `crates/medusa-agent/src/session.rs` | `crates/medusa-memory` |
| Production task contracts and mutation classification | `crates/medusa-runtime/src/production_orchestrator.rs` | `crates/medusa-multi-agent-scheduler` |

## Execute Safely

| Responsibility | Production status | Primary ownership | Supporting ownership |
|---|---|---|---|
| Read-only teammate scheduling | Shipped and called by production `run_prompt` | `crates/medusa-runtime/src/multi_agent_coordinator.rs`, `crates/medusa-multi-agent-scheduler` | `crates/medusa-agent/src/worker_execution.rs`, `crates/medusa-worker-leases` |
| Parent/teammate evidence integration | Shipped for planner and risk reviewer | `crates/medusa-runtime/src/multi_agent_coordinator.rs`, `crates/medusa-agent/src/team.rs` | `crates/medusa-agent` |
| Mutating worker isolation and integration | Shipped and called by production `run_prompt` for explicit mutation objectives | `crates/medusa-runtime/src/mutating_worker_coordinator.rs`, `crates/medusa-workers` | `crates/medusa-agent/src/worker_execution.rs`, `crates/medusa-multi-agent-scheduler` |
| Parent production review | Shipped as read-only coordinated lead | `crates/medusa-runtime`, `crates/medusa-agent` | `crates/medusa-provider` |
| Read-set and distributed transaction abstractions | Supporting, not the current production integration authority | `crates/medusa-worker-read-set`, `crates/medusa-worker-transaction` | `crates/medusa-transaction-coordinator` |
| Commit barrier and consensus | Design-only supporting paths | `crates/medusa-commit-barrier`, `crates/medusa-consensus` | `crates/medusa-conflict-resolution` |
| Filesystem transaction safety | Shipped | `crates/medusa-agent/src/transaction.rs` | `crates/medusa-repository-rollback` |
| Process containment | Shipped, platform-limited | `crates/medusa-process-containment` | `crates/medusa-process-registry` |
| Repository verification gate | Shipped | `crates/medusa-agent`, `crates/medusa-runtime` | `crates/medusa-hardening` |
| Shared runtime events | Shipped | `crates/medusa-protocol`, `crates/medusa-runtime` | `crates/medusa-tui`, `apps/medusa-desktop` |

Current coordinated execution constructs separate planner and risk-reviewer sessions. A mutating objective then creates one execution-specific implementer worktree, runs an implementer `AgentEngine` there, validates its changed paths and verification evidence, and integrates its deterministic commit. The parent session is read-only and owns review, reporting, and the final verification gate. Dynamic multi-implementer decomposition and autonomous team steering remain later promotion slices.

## Recover

| Responsibility | Primary ownership | Supporting ownership |
|---|---|---|
| Worker leases and restart epochs | `crates/medusa-agent/src/worker_execution.rs` | `crates/medusa-worker-leases` |
| Worktree crash recovery and cleanup | `crates/medusa-runtime/src/mutating_worker_coordinator.rs` | `crates/medusa-workers` |
| Checkpoints and replay | `crates/medusa-execution-checkpoint`, `crates/medusa-execution-replay` | `crates/medusa-time-travel` |
| Runtime supervision | `crates/medusa-runtime-supervisor` | `crates/medusa-daemon` |
| Recovery coordination | `crates/medusa-recovery-coordinator` | `crates/medusa-repository-rollback` |
| Durable memory and learning | `crates/medusa-memory`, `crates/medusa-markdown-memory` | `crates/medusa-memory-writeback`, `crates/medusa-memory-consolidation`, `crates/medusa-improvement` |

## Persisted authority by concern

| Concern | Owning paths | What downstream consumers may do |
|---|---|---|
| Plans | `crates/medusa-agent/src/session.rs`, `crates/medusa-runtime/src/production_orchestrator.rs` | Render, resume, and bind execution; never infer a replacement plan from UI state. |
| Execution | `crates/medusa-agent`, `crates/medusa-runtime`, `crates/medusa-workers` | Summarize actual session, worktree, commit, and integration evidence. |
| Verification | `crates/medusa-agent`, `crates/medusa-runtime`, browser crates | Decide completion from recorded required checks and overrides. |
| Reports | Runtime/session reporting paths | Derive a human-readable result from authoritative execution and verification records. |
| Learning | Memory, Markdown memory, writeback, consolidation, and improvement crates | Accept positive learning only from verified outcomes with provenance. |
| Recovery | Worker execution, checkpoint, replay, rollback, failure, and recovery coordinator paths | Resume, retry, or roll back while retaining interruption and failure history. |

## Capability status source

Production claims and executable evidence are indexed in [`CAPABILITY-CLAIMS.json`](CAPABILITY-CLAIMS.json) and rendered in [`CAPABILITY-EVIDENCE.md`](CAPABILITY-EVIDENCE.md). When a product capability changes, update production code, behavioral tests, the claims manifest, the evidence ledger, the execution trace, and this map.
