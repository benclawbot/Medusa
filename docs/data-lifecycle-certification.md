# Data lifecycle and privacy certification

This document is the repository-wide lifecycle/privacy authority required by issue #777. The executable inventory lives in `medusa-testkit::data_lifecycle`; production storage remains owned by the subsystem named in the matrix rather than by a second persistence layer.

## Non-negotiable rules

1. Canonical journal/state is authoritative. Rebuildable projections, indexes, summaries, caches, and content-addressed objects are derived and may not silently outlive the source policy that authorizes them.
2. Credentials, raw secret values, ambient environment secrets, hidden reasoning, and provider-internal reasoning blocks are never approved durable data classes.
3. A content hash, artifact ID, cache key, stale handle, preview URL, or local frontend identifier is never authorization. Retrieval must remain inside the owning session/repository/user trust scope.
4. Deletion is not complete while a user-visible scope can still recover the data from a projection, index, cache, snapshot, content-addressed object, frontend cache, exported Medusa-owned bundle, or crash-remnant path.
5. Garbage collection follows authoritative references. Corrupt/ambiguous reachability fails safe: preserve authoritative data, quarantine bounded orphan diagnostics, and surface degradation instead of speculatively deleting.
6. Disk-pressure cleanup may evict only declared derived/ephemeral priority classes. It cannot opportunistically delete canonical recovery, rollback, verification, or security-critical state.
7. Export is bounded, redacted, versioned, scope-preserving, and non-mutating. Intentional omissions must appear in the export manifest.
8. Raw live microphone/audio is ephemeral by default. A feature that makes it durable must add a lifecycle entry and explicit user-facing policy before landing.

## Lifecycle matrix

| Data class | Production owner / authority | Retention and GC | Export / redaction | Deletion and backup semantics | Visibility |
|---|---|---|---|---|---|
| Session journal/events | `medusa-agent::journal`, `medusa-protocol` / authoritative | Session-scoped; collect only after required recovery/audit references are gone | Exportable; secrets excluded | Tombstone then GC; backups inherit session disposition and cannot rehydrate a deleted live scope | Owning session + authorized projections |
| Execution checkpoints | `medusa-execution-checkpoint`, `medusa-runtime::checkpoint_store` / derived | While referenced; collect superseded checkpoints after recovery references disappear | Exportable; secrets excluded | Scope-bound GC; backup follows owning recovery window | Owning session |
| Materialized projections | `medusa-runtime`, `medusa-execution-replay` / derived | Rebuildable; invalidate on source change/disposition | Not independently exported | Scope-bound GC; rebuild from live authority rather than restore after source deletion | Owning session/repository |
| Compaction manifests/summaries | `medusa-agent::compaction_v2`, `medusa-context` / derived | Never outlive source range/session | Exportable; secrets excluded | Scope-bound GC; backup cannot extend source retention | Owning session |
| Time-travel branch summaries | `medusa-time-travel`, `medusa-agent::branch_summary` / derived | Collect after branch abandonment/merge and last recovery reference | Exportable; secrets excluded | Scope-bound GC including branch-local summaries/artifacts | Owning session/repository |
| Frontend history/transcripts | `medusa-runtime::frontend`, observer, voice / derived | Session-scoped | Exportable; secrets excluded | Frontend caches are invalidated with session disposition | Owning session + authorized frontend |
| Tool/model artifacts | `medusa-evidence::ArtifactStore` / derived | While an authorized live reference exists | Exportable; secrets excluded | Last-reference GC; deduplicated object survives only for other authorized live references; hash is never authorization | Reference-owning session/repository/user scope |
| Evidence/receipts/diffs/diagnostics/logs | `medusa-evidence`, runtime verification authorities / derived | Repository/session scoped subject to declared security evidence | Redacted export | Scope-bound GC; retain only required security window | Owning repository + authorized session |
| Analysis workspaces/exports | `medusa-runtime::analysis_workspace` / derived | Session/workspace scoped | Redacted/versioned export | Medusa-owned copies follow workspace deletion; delivered copies become user-managed | Owning workspace/session |
| Refinement/learning state | `medusa-improvement::RefinementAuthorityStore` at `.medusa/refinement-authority` / authoritative | User/repository scoped; journal and receipts retained for declared rollback/security evidence | Exportable, versioned, redacted; selection provenance included | Tombstone then GC while preserving direct-predecessor and security evidence; `active.json` is rebuildable | Owning user/repository |
| Skills/packages/provenance | extension/runtime skill authorities / derived | Repository/user scoped | Exportable; secrets excluded | Scope-bound GC; package identity/hash may survive only where required provenance is non-private | Owning repository/user + authorized execution |
| Scheduled/session actions | runtime scheduled actions + `SessionAction` journal / authoritative | Session scoped | Exportable; secrets excluded | Tombstone then GC with session | Owning session/user |
| Provider/OAuth metadata | config/provider authorities / authoritative metadata only | User scoped until disconnect/reset | Metadata-only export | Immediate removal permitted for disconnected metadata; credential material is never in approved backup state | Owning config scope |
| Voice/realtime/Telegram evidence | runtime voice/realtime/frontend adapters / derived | Session scoped; raw live buffers released at turn/session end | Sanitized/redacted export | Scope-bound GC; raw audio is not backed up by default | Owning session/user |
| Configuration history/audit | config + runtime configuration events / authoritative | Repository/user scoped subject to audit requirements | Redacted export | Tombstone then GC; backup is redacted | Owning repository/user |
| Crash/support bundles | explicit support/diagnostic authority / exported | Bounded Medusa-owned retention, maximum 30 days | Redacted/versioned | Immediate explicit delete; retained Medusa copy expires | Requesting user/support scope |
| Memory Markdown + index | `medusa-memory` / Markdown authoritative, SQLite/search rebuildable | User/repository scoped | Exportable; secrets excluded | Deleting/updating authority invalidates/rebuilds derived index; index cannot resurrect deleted content | Owning memory scope |
| Prompt/MCP/context caches | prompt/MCP/context retrieval crates / derived | Bounded; maximum 30 days, or earlier invalidation | Not independently exported | Scope-bound cleanup on expiry/source deletion/disk pressure; key/hash is never authorization | Same trust scope as source |
| Temporary worktrees/files/resource-pool state | containment/workers/runtime / ephemeral | Transaction/session only, maximum 1 day crash-remnant bound | Not exported | Immediate deterministic cleanup/reconciliation; never intentionally backed up | Owning transaction/session |

The exact machine-readable fields—owner, authority, storage, provenance, default/maximum retention, GC trigger, exportability, redaction, deletion, backup implications, and cross-scope visibility—are validated by `crates/medusa-testkit/src/data_lifecycle.rs`.

## Deletion and deduplication contract

A session/repository/user deletion operation must first remove or tombstone the authoritative scope, then invalidate every derived lookup path for that scope. Shared content-addressed bytes may remain only while a different authorized live reference exists. Removing the final authorized reference makes the blob collectible. A stale artifact ID or known hash must return not-found/unauthorized after its scope reference is removed even if identical bytes remain for another scope.

Secondary indexes and projections are never deletion authorities. If their source is gone, rebuild/reconciliation removes the orphan; corruption cannot make the orphan visible. Frontends and observer/side-question routes apply the same rule rather than treating cached identifiers as capabilities.

## Export contract

Supported exports include a schema version, scope/provenance metadata, a manifest of included data classes, redaction status, and explicit excluded items. Export creation reads source state without mutating retention, reachability, tombstones, or GC eligibility. Credentials, raw secret values, hidden/provider reasoning, and unapproved raw audio are excluded rather than merely masked after serialization.

## Crash and corruption behavior

GC/deletion is idempotent and reconciled after interruption. When reachability cannot be proven, authoritative data is preserved. Rebuildable projections/indexes are reconstructed from live authority. Corrupt orphan candidates may be quarantined only with bounded diagnostics and cannot become visible through ordinary retrieval.

## CI enforcement

`.github/workflows/data-lifecycle-certification.yml` runs the lifecycle inventory, canonical refinement authority policy, migration, and recovery suites plus the highest-value production storage/recovery suites on Linux, macOS, and Windows. Changes to durable-state owners, caches, frontends, evidence/artifacts, memory, checkpoints, time travel, refinement, configuration, or lifecycle policy trigger the gate.

Any new durable/derived class must add a `LifecycleEntry` before merge. The entry is a declaration, not a waiver: production behavior and integration tests must still prove deletion, GC, export, isolation, minimization, and recovery semantics for the paths the class exposes.
