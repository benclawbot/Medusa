# Unified Refinement Authority Design

> Historical record — retained as implementation evidence; it is not current setup or status guidance. Start at [the documentation index](../../README.md).

Status: Accepted for implementation after user review

Issue: [#819](https://github.com/benclawbot/Medusa/issues/819)

Date: 2026-08-12

## Objective

Medusa will have one durable authority for learned behavior. An explicitly approved refinement will become visible to the real production runtime before a matching turn begins, while nonmatching turns remain unaffected. The authority will preserve proposal identity, source evidence, trust, scope, validation, evaluation, approval, activation, suspension, supersession, rollback, deletion, migration, and recovery history.

The implementation will close issue #819 in one pull request. It will not implement correction synthesis, causal monitoring, meta-improvement, or the final Factory Dashboard redesign owned by issues #821, #822, #823, and #824. It will provide the shared authority those issues consume and will prevent legacy state from claiming production activation independently.

## Assumptions and constraints

The append-only refinement lifecycle introduced by #759 remains the semantic engine. `medusa-context` stays deterministic and filesystem-independent. The repository-wide lifecycle policy merged by #777 assigns durable refinement ownership to `medusa-improvement`, so persistence, locking, projection materialization, migration, export, and quarantine belong there. `medusa-runtime` remains the production command and turn-assembly boundary. Existing frontend commands continue to enter through `medusa-runtime`; frontends do not gain direct filesystem authority.

Approval is not inferred from a status string, a legacy file, or model output. The explicit `/learning approve` command creates a proposal-bound durable receipt in the canonical authority. Activation revalidates that receipt after deserialization and immediately before projection commit. Executable skill installation remains governed by #760; this change records and projects skill lifecycle state without making unverified code executable.

No new third-party dependency is planned. Rust 1.88 and edition 2024 remain the workspace floor. All filesystem behavior must work on Linux, macOS, and Windows.

## Decision and alternatives

The selected design places `RefinementAuthorityStore` in `medusa-improvement`. It wraps `medusa_context::refinement::RefinementJournal`, owns durable transactions, and exposes typed snapshots and transitions. `medusa-runtime` uses this store for all learning commands and pre-turn selection. This follows the authority assignment from #777, preserves the pure lifecycle engine from #759, and creates one interface for every frontend.

An alternative was to put persistence directly in `medusa-context`. That would make context assembly own locking, migration, privacy, and storage policy, and would turn a deterministic model crate into an application service. It is rejected.

Another alternative was to promote `ScopedMemoryStore` and `learnings.json` to the canonical authority. That store already feeds runtime retrieval, but it lacks the complete evaluated and approved refinement lifecycle and would leave the #759 journal as another disconnected authority. It is rejected.

## Ownership and files

The canonical repository-scoped root is `.medusa/refinement-authority`. `journal.json` contains the versioned append-only `RefinementJournal` and its hash-linked lifecycle entries. `approvals.json` contains proposal-bound approval records issued only through the shared command service. `active.json` is a rebuildable materialized projection with the source journal head hash, projection revision, active proposal IDs and versions, scope selectors, content, and provenance. `migrations.jsonl` records one-way import receipts. `quarantine` stores bounded copies or metadata for corrupt and ambiguous legacy records that cannot be admitted safely.

The journal and approvals are authoritative. The active projection, compatibility views, selection audit, and frontend snapshots are derived. A projection never authorizes behavior by itself. At open, resume, and before each relevant turn, the store validates the journal chain, revalidates approval receipts, rebuilds or verifies `active.json`, and returns a typed selection. A corrupt authority fails closed and is quarantined without applying any refinement. A missing or corrupt projection is rebuilt from valid authority.

`medusa-context` will add lifecycle events needed by the issue but will not perform I/O. The event model will support proposal, validation, evaluation, approval, activation, supersession, suspension, rollback, rejection, and tombstone. Projection state will distinguish active, inactive, suspended, rolled back, rejected, deleted, and conflicted records. Existing serialized events remain readable through serde defaults or explicit schema migration.

`medusa-improvement` will add the durable store, approval binding, selection, migration, export, and quarantine implementation. Existing `LearningReviewStore` and `ScopedMemoryStore` APIs remain temporarily readable as compatibility inputs, but production mutation and activation move to `RefinementAuthorityStore`. Their write paths will either delegate to the canonical service or return a structured deprecation error; they cannot publish active production behavior independently.

`medusa-runtime` will replace direct `LearningReviewStore` transitions and `ScopedMemoryStore` retrieval with a shared `learning_authority` service. `/learning show`, `approve`, `reject`, `defer`, `validate`, `activate`, `suspend`, `rollback`, `delete`, `privacy`, and `export` will operate on canonical typed state. The service will supply prompt context plus a selection audit containing canonical proposal ID, version, scope, source evidence IDs, approval receipt ID, and journal head hash. Startup/resume performs recovery; every relevant turn refreshes selection before prompt assembly.

Desktop, TUI, CLI-backed runtime, daemon, and Telegram will continue to use shared runtime commands and events. The desktop engineering file remains a legacy input until #824, but its records are imported only as untrusted candidates and its local `active` or `adopted` values do not make behavior active. Any lifecycle control that cannot route through the canonical service returns a truthful unavailable or migration-required result.

## Public contracts

The primary application interface will be `RefinementAuthorityStore::open(repo: &Path) -> Result<Self, RefinementAuthorityError>`. Read operations will include `snapshot()`, `select(&SelectionContext)`, `export()`, and `migration_status()`. Transition operations will accept an expected revision and return the committed canonical snapshot: `propose`, `validate`, `record_evaluation`, `approve`, `activate`, `supersede`, `suspend`, `rollback`, `reject`, and `tombstone`. Stale expected revisions return `Conflict` without writing.

`ApprovalBinding` will include the proposal ID, proposal version, proposal digest, explicit actor class, command or decision identifier, issuance time, and receipt digest. `DurableApprovalAuthority` will verify all bindings against authoritative approval records and the exact proposal content. Deserializing a journal does not populate trusted approvals; the store must revalidate them before projection or activation.

`SelectionContext` will carry repository identity, optional user and session identity, task and artifact kinds, normalized context tags, explicit exclusions, objective text, and current time. `SelectedRefinement` will carry the canonical ID and version, artifact kind, scope, content, evidence IDs, approval receipt ID, selection rationale, and journal head hash. Selection is deterministic. Equal-precedence conflicts fail closed and appear in the snapshot and runtime notice rather than using last-write-wins.

The runtime service will expose one frontend-neutral snapshot shape. Existing `LearningReviewSnapshot` consumers will receive a compatibility projection derived from the canonical snapshot during the migration window. New frontend-specific lifecycle state or storage paths are prohibited by the architecture policy test.

## Transaction and recovery model

Every mutation acquires one repository-scoped lifecycle lock using the established cross-platform create-new lock and bounded stale-owner recovery pattern already used by Medusa stores. Under the lock, the store reloads authority, validates the expected revision, validates the proposed transition, writes the new authoritative documents durably, materializes the candidate active projection, validates that the candidate can be selected through the production selector, and atomically replaces the visible projection. Activation is reported only after all these operations succeed.

If authority persistence fails, neither authority nor projection advances. If projection materialization fails after an authoritative event is prepared, the transaction does not publish activation and recovery reconciles the temporary transaction on the next open. Atomic replacement uses same-directory temporary files, file synchronization, rename or Windows-safe replacement, and parent-directory synchronization where supported.

Rollback names the exact direct predecessor recorded by the supersession edge. It restores that predecessor, writes a new projection revision, and changes the journal head hash used by prompt-cache provenance. A request to restore a non-direct ancestor fails. Suspension removes future exposure without deleting evidence or lineage. Tombstoning removes future behavior and private content according to #777 while retaining the minimum non-sensitive audit identity required for lineage and deletion certification.

## Migration and compatibility

Migration is idempotent and one-way. Each legacy source is fingerprinted, imported once, and assigned a receipt that records its source path, source record identity, canonical proposal identity, disposition, original timestamps, and redaction result. Missing approval, evaluation, scope, provenance, or stable identity is never invented.

| Legacy source | Canonical disposition |
|---|---|
| `.medusa/learning-review` | Import proposals, review decisions, privacy, replay, and audit provenance. Only verifiable explicit approvals become approval bindings; a legacy active state without a valid binding becomes a non-active reviewed candidate. |
| Repository and user `learnings.json` | Import active rules as legacy candidates with original applicability, confidence, scope, provenance, and tombstones. They require canonical review unless a matching verifiable approval receipt exists. Runtime stops reading these files directly after migration. |
| Session lesson proposals and memory lessons | Preserve source session and evidence identifiers as proposed or migrated memory refinements. Existing Markdown memory authority remains separate and is referenced rather than copied as active prompt guidance. |
| Skill proposals, installed skills, outcomes, reviews, quarantine, probation, and graduation | Preserve package identity, hashes, lifecycle receipts, scope, outcomes, and review provenance. Executability remains inactive unless #760 verification authority permits it. |
| `ImprovementStore` and hardening feedback | Import as typed candidates and evaluations where schemas prove the mapping; fixture-only benchmark responses are not treated as production evaluation. |
| `.medusa/engineering/improvements.json` | Import as untrusted legacy candidates. `active`, `adopted`, `benchmarked`, and rollback strings do not imply canonical approval, evaluation, or activation. |
| Refinement journal fixtures or prior canonical data | Validate and import losslessly when the hash chain and schema are valid; quarantine corrupt tails and preserve the last valid prefix for recovery inspection. |

Compatibility projections are read-only and rebuilt from canonical state. Legacy writers receive a structured deprecation result after their migration adapter is installed. The migration window ends when #824 moves the Factory Dashboard and #760 moves executable skill activation to the shared services; deletion of legacy files is not part of #819 unless a file is proven unused and safely migrated.

## Privacy and security

The #818 admission policy gates every proposal before durable persistence. Capture-disabled repositories do not open or migrate user learning state. Cross-repository reuse requires its explicit policy flag. Secret-bearing content, hidden reasoning, raw images, credential material, and untrusted repository or web instructions cannot become active refinements. Migration applies the same admission and redaction rules and quarantines violations.

Refinement content cannot change containment, approval authority, capability policy, credential handling, verification authority, update trust, repository mutation authority, or protected system prompts. Prompt guidance that would change effective capability is rejected from the runtime refinement lane. Executable packages are metadata-only until the separate contained verification authority accepts them.

Known hashes, proposal IDs, projection revisions, and migration receipts are not capabilities. All reads remain bound to repository, user, organization, workspace, session, and task scope. Export is redacted, versioned, non-mutating, and includes explicit omissions.

## Runtime data flow

An explicit correction or existing learning source enters the admission policy and becomes a canonical proposal with evidence and scope. Validation and evaluation append canonical events. `/learning approve` records a proposal-bound explicit approval. `/learning activate` revalidates the proposal, evaluation, approval, expected revision, scope conflicts, and protected-root policy, then commits authority and the active projection atomically.

Before a later turn, runtime opens or refreshes the authority, validates or rebuilds the projection, selects only matching conflict-free refinements, and appends their rendered content to task context. The prompt provenance and selection audit contain the canonical IDs and source journal hash. A nonmatching task receives no content. Suspension, rollback, or deletion changes the projection and its provenance, so stale prompt-cache entries cannot continue applying the removed version.

## Error semantics

Errors are typed as invalid input, admission denied, approval required, evaluation required, stale revision conflict, scope conflict, protected target, corrupt authority, corrupt legacy source, migration conflict, lock unavailable, persistence failure, projection failure, and unsupported legacy mutation. User-facing runtime notices distinguish unavailable, blocked, quarantined, and conflicted state without coercing them to success.

Authority corruption fails closed. Projection corruption is recoverable from authority. Ambiguous legacy records are quarantined. A transient write error leaves the prior active projection visible and does not report the candidate active. Concurrent or stale transitions cannot create two active successors.

## Testing strategy

The implementation follows red-green-refactor. Unit tests in `medusa-context` prove lifecycle transitions, direct-predecessor rollback, suspension, tombstone, hash validation, and deterministic conflicts. Unit and integration tests in `medusa-improvement` prove durable locking, atomic activation, approval revalidation, projection rebuild, corruption quarantine, privacy gates, selection, export, and every migration fixture. Runtime integration tests use the real turn-context assembly to prove that an approved correction changes a later matching task, does not affect a nearby nonmatching task, survives restart, and disappears after suspension or rollback.

Contract tests prove that Desktop, TUI, CLI/runtime, daemon, and Telegram observe the same canonical revision and lifecycle state. Architecture policy tests reject new frontend-owned or legacy writable improvement authorities. Cross-platform CI exercises crash-safe persistence and migration on Linux, macOS, and Windows.

Focused verification runs `cargo fmt --all -- --check`, `cargo test -p medusa-context`, `cargo test -p medusa-improvement`, and the narrow runtime integration tests first. Completion verification runs `cargo clippy --workspace --all-targets --all-features -- -D warnings` followed by `cargo test --workspace --all-targets --all-features`. CI failures are collected across all jobs and fixed in one batch before the next push-triggered run.

## Boundaries

Always preserve public compatibility during the migration window, validate at external and persistence boundaries, use structured errors, keep authority changes append-only, make projections rebuildable, bind approvals to exact proposal content, and add regression coverage before production code.

Ask before adding a third-party dependency, changing the supported Rust version, changing the user-approved issue scope, removing a legacy file before its consumers are migrated, or weakening an existing approval or privacy rule.

Never infer approval or activation from legacy strings, let a frontend write lifecycle state directly, treat hashes or IDs as authorization, execute migrated skill code, apply untrusted repository or web instructions as policy, expose secret-bearing content, use last-write-wins for scope conflicts, or publish an active projection before the authoritative transaction succeeds.

## Success criteria

Issue #819 is complete when one canonical authority owns all active learned behavior; an explicitly approved repository refinement changes a later matching production runtime turn without affecting a nonmatching turn; startup and resume revalidate approval and reconstruct the same projection; corrupt projection recovery and corrupt authority quarantine are proven; stale and concurrent transitions fail safely; rollback restores the direct predecessor; every listed legacy store has a tested migration or compatibility disposition; all frontends consume one canonical revision; no legacy or frontend store can independently claim active behavior; the architecture policy prevents a new competing authority; focused and workspace validation pass; and the dedicated PR is merged to `main` after green CI.

## Open questions

There are no unresolved product choices. Implementation discoveries that materially change authority, approval, persistence, migration, or issue scope require a design update and renewed user approval before code changes continue.
