# Contributor Architecture Map

This map connects the product architecture in [`ARCHITECTURE.md`](ARCHITECTURE.md) to authoritative Rust crates and entrypoints. It is intentionally secondary to the product model: contributors use the crate graph to find ownership, while users reason about **Plan, Execute Safely, Recover**.

## Runtime entrypoints

| Surface | Authoritative path | Responsibility |
|---|---|---|
| Production orchestration | `medusa-runtime::production_orchestrator` | Owns the production execution model and shared runtime event flow. |
| Terminal UI | `crates/medusa-tui` | Renders and drives the shared runtime interactively. |
| Desktop application | `apps/medusa-desktop` | React/Tauri frontend over the shared runtime and daemon boundary. |
| Headless CLI | `crates/medusa-cli` | Starts scripted objectives, resume flows, maintenance commands, and explicit approval allowlists. |
| Agent session engine | `crates/medusa-agent` | Session state, plans, approvals, transactions, tool use, verification wiring, and durable records. |

The workspace metadata in the root `Cargo.toml` is the machine-readable authority for the production execution model, orchestrator, delegation contract, and verification gate.

## Plan

| Responsibility | Primary ownership | Supporting ownership |
|---|---|---|
| Objective and goal state | `crates/medusa-goal`, `crates/medusa-world-model` | `crates/medusa-context`, `crates/medusa-context-retrieval` |
| Turn and prompt assembly | `crates/medusa-turn-assembly`, `crates/medusa-agent` | `crates/medusa-prompt-cache`, `crates/medusa-provider` |
| Progress and confidence | `crates/medusa-progress`, `crates/medusa-confidence` | `crates/medusa-intelligence` |
| Plan-bound approval | `crates/medusa-agent/src/approval.rs` | `crates/medusa-agent/src/identity_guard.rs` |
| Persisted session and plan state | `crates/medusa-agent/src/session.rs` | `crates/medusa-memory` |

## Execute Safely

| Responsibility | Primary ownership | Supporting ownership |
|---|---|---|
| Production orchestration | `crates/medusa-runtime` | `crates/medusa-execution-orchestrator` |
| Multi-agent scheduling | `crates/medusa-multi-agent-scheduler` | `crates/medusa-workers`, `crates/medusa-worker-leases` |
| Parent/subagent integration | `crates/medusa-runtime`, `crates/medusa-agent` | `crates/medusa-consensus`, `crates/medusa-conflict-resolution` |
| Read-set and isolated mutation | `crates/medusa-worker-read-set`, `crates/medusa-worker-transaction` | `crates/medusa-transaction-coordinator` |
| Commit barrier | `crates/medusa-commit-barrier` | `crates/medusa-repository-snapshot` |
| Filesystem transaction safety | `crates/medusa-agent/src/transaction.rs` | `crates/medusa-repository-rollback` |
| Process containment | `crates/medusa-process-containment` | `crates/medusa-process-registry` |
| Browser verification | `crates/medusa-browser-client`, `crates/medusa-browserd` | `crates/medusa-runtime` |
| Repository verification gate | `crates/medusa-agent`, `crates/medusa-runtime` | `crates/medusa-hardening` |
| Shared runtime events | `crates/medusa-protocol`, `crates/medusa-runtime` | `crates/medusa-tui`, `apps/medusa-desktop` |

A subagent result is input to the primary agent, not a completed change. The primary agent validates evidence, integrates accepted work, resolves conflicts, and submits the combined repository state to the commit and verification gates.

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
| Execution | `crates/medusa-agent`, `crates/medusa-runtime`, transaction and process crates | Summarize tool and mutation evidence; never treat proposed text as applied work. |
| Verification | `crates/medusa-agent`, `crates/medusa-runtime`, browser crates | Decide completion from recorded required checks and overrides. |
| Reports | Runtime/session reporting paths | Derive a human-readable result from authoritative execution and verification records. |
| Learning | Memory, Markdown memory, writeback, consolidation, and improvement crates | Accept positive learning only from verified outcomes with provenance. |
| Recovery | Checkpoint, replay, supervisor, rollback, failure, and recovery coordinator crates | Resume, retry, or roll back while retaining interruption and failure history. |

## Capability status source

Production claims and executable evidence are indexed in [`CAPABILITY-CLAIMS.json`](CAPABILITY-CLAIMS.json) and rendered in [`CAPABILITY-EVIDENCE.md`](CAPABILITY-EVIDENCE.md). When a product capability changes, update production code, behavioral tests, the claims manifest, the evidence ledger, and this map when ownership or entrypoints change.
