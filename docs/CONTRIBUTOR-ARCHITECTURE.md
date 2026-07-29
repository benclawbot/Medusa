# Contributor Architecture Map

This map connects the product architecture in [`ARCHITECTURE.md`](ARCHITECTURE.md) to authoritative Rust crates and entrypoints. It is intentionally secondary to the product model: contributors use the crate graph to find ownership, while users reason about **Plan, Execute Safely, Recover**.

## Runtime entrypoints

| Surface | Authoritative path | Responsibility |
|---|---|---|
| Shared production runtime | `medusa-runtime::RuntimeController -> run_prompt` | Owns shared runtime events, provider construction, cancellation, follow-ups, session continuity, and coordinated-mode selection. |
| Production agent engine | `medusa-agent::AgentEngine` | Advances one authoritative `AgentSession`; owns plans, approvals, transactions, tool use, verification wiring, and durable records. |
| Production multi-agent coordinator | `run_prompt -> multi_agent_coordinator::run_preflight` | Owns bounded read-only teammate dispatch, durable leases, team lifecycle, evidence handoff, cancellation, and the coordinated verification gate. |
| Terminal UI | `crates/medusa-tui` | Renders and drives the shared runtime interactively. |
| Desktop application | `apps/medusa-desktop` | React/Tauri frontend over the shared runtime and daemon boundary. |
| Headless CLI | `crates/medusa-cli` | Starts scripted objectives, resume flows, maintenance commands, and explicit approval allowlists. |

The workspace metadata in the root `Cargo.toml` is the machine-readable authority for the production execution model, entrypoint, read-only delegation boundary, parent mutation authority, and verification gate. The complete trace is in [`PRODUCTION-EXECUTION-TRACE.md`](PRODUCTION-EXECUTION-TRACE.md).

## Plan

| Responsibility | Primary ownership | Supporting ownership |
|---|---|---|
| Objective and goal state | `crates/medusa-goal`, `crates/medusa-world-model` | `crates/medusa-context`, `crates/medusa-context-retrieval` |
| Turn and prompt assembly | `crates/medusa-turn-assembly`, `crates/medusa-agent` | `crates/medusa-prompt-cache`, `crates/medusa-provider` |
| Progress and confidence | `crates/medusa-progress`, `crates/medusa-confidence` | `crates/medusa-intelligence` |
| Plan-bound approval | `crates/medusa-agent/src/approval.rs` | `crates/medusa-agent/src/identity_guard.rs` |
| Persisted session and plan state | `crates/medusa-agent/src/session.rs` | `crates/medusa-memory` |
| Production task contracts and first-wave scheduling | `crates/medusa-runtime/src/production_orchestrator.rs` | `crates/medusa-multi-agent-scheduler` |

## Execute Safely

| Responsibility | Production status | Primary ownership | Supporting ownership |
|---|---|---|---|
| Parent production execution | Shipped | `crates/medusa-runtime`, `crates/medusa-agent` | `crates/medusa-provider` |
| Read-only teammate scheduling | Shipped and called by production `run_prompt` | `crates/medusa-runtime/src/multi_agent_coordinator.rs`, `crates/medusa-multi-agent-scheduler` | `crates/medusa-agent/src/worker_execution.rs`, `crates/medusa-worker-leases` |
| Parent/teammate evidence integration | Shipped for read-only planner and risk reviewer | `crates/medusa-runtime/src/multi_agent_coordinator.rs`, `crates/medusa-agent/src/team.rs` | `crates/medusa-agent` |
| Read-set and isolated worker mutation | Design-only supporting paths | `crates/medusa-worker-read-set`, `crates/medusa-worker-transaction` | `crates/medusa-transaction-coordinator` |
| Commit barrier | Design-only supporting path | `crates/medusa-commit-barrier` | `crates/medusa-repository-snapshot` |
| Filesystem transaction safety | Shipped | `crates/medusa-agent/src/transaction.rs` | `crates/medusa-repository-rollback` |
| Process containment | Shipped, platform-limited | `crates/medusa-process-containment` | `crates/medusa-process-registry` |
| Browser verification | Shipped, prerequisite-limited | `crates/medusa-browser-client`, `crates/medusa-browserd` | `crates/medusa-runtime` |
| Repository verification gate | Shipped | `crates/medusa-agent`, `crates/medusa-runtime` | `crates/medusa-hardening` |
| Shared runtime events | Shipped | `crates/medusa-protocol`, `crates/medusa-runtime` | `crates/medusa-tui`, `apps/medusa-desktop` |

Current coordinated execution constructs separate read-only planner and risk-reviewer `AgentEngine` sessions before the parent session. Durable leases, team records, and validated evidence prove that dispatch occurred. Mutating worker, consensus, commit-barrier, and distributed transaction APIs remain non-production until they are reachable from this coordinator.

## Recover

| Responsibility | Primary ownership | Supporting ownership |
|---|---|---|
| Checkpoints | `crates/medusa-execution-checkpoint` | `crates/medusa-agent` |
| Replay | `crates/medusa-execution-replay` | `crates/medusa-time-travel` |
| Runtime supervision | `crates/medusa-runtime-supervisor` | `crates/medusa-daemon` |
| Continuation and wakeup | `crates/medusa-continuation`, `crates/medusa-wakeup` | `crates/medusa-progress` |
| Failure and escalation | `crates/medusa-failure`, `crates/medusa-escalation` | `crates/medusa-confidence` |
| Recovery coordination | `crates/medusa-recovery-coordinator` | `crates/medusa-repository-rollback` |
| Durable memory and learning | `crates/medusa-memory`, `crates/medusa-markdown-memory` | `crates/medusa-memory-writeback`, `crates/medusa-memory-consolidation`, `crates/medusa-improvement` |

## Persisted authority by concern

| Concern | Owning paths | What downstream consumers may do |
|---|---|---|
| Plans | `crates/medusa-agent/src/session.rs`, `crates/medusa-goal` | Render, resume, and bind approvals; never infer a replacement plan from UI state. |
| Execution | `crates/medusa-agent`, `crates/medusa-runtime`, transaction and process crates | Summarize actual tool and mutation evidence; never treat proposed text, task contracts, roles, or schedule waves as applied or delegated work. |
| Verification | `crates/medusa-agent`, `crates/medusa-runtime`, browser crates | Decide completion from recorded required checks and overrides. |
| Reports | Runtime/session reporting paths | Derive a human-readable result from authoritative execution and verification records. |
| Learning | Memory, Markdown memory, writeback, consolidation, and improvement crates | Accept positive learning only from verified outcomes with provenance. |
| Recovery | Checkpoint, replay, supervisor, rollback, failure, and recovery coordinator crates | Resume, retry, or roll back while retaining interruption and failure history. |

## Capability status source

Production claims and executable evidence are indexed in [`CAPABILITY-CLAIMS.json`](CAPABILITY-CLAIMS.json) and rendered in [`CAPABILITY-EVIDENCE.md`](CAPABILITY-EVIDENCE.md). When a product capability changes, update production code, behavioral tests, the claims manifest, the evidence ledger, the execution trace, and this map when ownership or entrypoints change.
