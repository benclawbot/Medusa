# Unified Refinement Authority Implementation Plan

> Historical record — retained as implementation evidence; it is not current setup or status guidance. Start at [the documentation index](../../README.md).

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make one durable refinement authority own every production-active learned behavior and apply approved, scope-matching refinements through the real runtime.

**Architecture:** `medusa-context` remains the pure append-only lifecycle engine. `medusa-improvement` owns durable journal, approval, projection, migration, and quarantine state under `.medusa/refinement-authority`; `medusa-runtime` is the only production command and prompt-selection boundary. Legacy stores become one-way migration inputs or read-only compatibility projections.

**Tech Stack:** Rust 1.88, edition 2024, serde/serde_json, sha2, time, existing Medusa error and persistence patterns, Cargo workspace tests.

## Global Constraints

No new third-party dependency is planned. Rust 1.88 and edition 2024 remain the workspace floor. All filesystem behavior must work on Linux, macOS, and Windows. Approval, privacy, containment, capability, credential, verification, update-trust, and repository-mutation boundaries may not be weakened. Production code follows red-green-refactor. The issue is delivered in one PR and CI findings are collected and fixed in one batch before the next run.

---

### Task 1: Complete the pure refinement lifecycle

**Files:**

Modify `crates/medusa-context/src/refinement.rs`, `crates/medusa-context/src/refinement_api.rs`, and `crates/medusa-context-retrieval/tests/refinement_production_path.rs`.

**Interfaces:**

This task consumes the existing `RefinementJournal`, `RefinementEvent`, `ApprovalAuthority`, and `RefinementProjection`. It produces `RefinementEvent::Deferred`, `RefinementEvent::Suspended`, and `RefinementEvent::Tombstoned`, plus public `RefinementLifecycle`, `RefinementRecord`, `RefinementProjection::records()`, `RefinementProjection::head_hash()`, and deterministic conflict reporting. Existing event JSON remains readable.

- [ ] **Step 1: Write lifecycle regression tests before production changes.** Add tests that build a valid evaluated and approved proposal, prove suspension removes it from `active()` without deleting lineage, prove rollback restores only the direct predecessor, prove tombstone redacts content and prevents reactivation, and prove two equal-precedence active successors are reported as conflicts rather than selected.

```rust
assert_eq!(projection.active(), Vec::<&RefinementProposal>::new());
assert_eq!(projection.records()[0].lifecycle, RefinementLifecycle::Suspended);
assert_eq!(projection.conflict_keys(), vec!["repository_convention:testing.workflow"]);
```

- [ ] **Step 2: Run the focused tests and confirm the expected RED failures.** Run `cargo test -p medusa-context refinement -- --nocapture` and `cargo test -p medusa-context-retrieval --test refinement_production_path -- --nocapture`. Expected failures are missing event variants and projection APIs, not fixture or compilation mistakes.

- [ ] **Step 3: Implement the minimal lifecycle state machine.** Extend both the core and public facade variants, transition validation, replay, serde conversion, and projection records. `Tombstoned` must retain only ID, version, artifact kind, scope, receipt/evidence digests, timestamps, and predecessor lineage; it must not retain active content.

```rust
pub enum RefinementLifecycle {
    Proposed,
    Deferred,
    Validated,
    Evaluated,
    Approved,
    Active,
    Superseded,
    Suspended,
    RolledBack,
    Rejected,
    Tombstoned,
    Conflict,
}
```

- [ ] **Step 4: Run focused tests and formatting.** Run `cargo fmt --all -- --check`, `cargo test -p medusa-context`, and `cargo test -p medusa-context-retrieval --test refinement_production_path`.

- [ ] **Step 5: Commit the independently reviewable lifecycle change.** Stage only the context files and commit with `feat(context): complete refinement lifecycle`.

### Task 2: Add the durable canonical authority and atomic projection

**Files:**

Create `crates/medusa-improvement/src/refinement_authority.rs`, `crates/medusa-improvement/src/refinement_persistence.rs`, and `crates/medusa-improvement/tests/refinement_authority.rs`. Modify `crates/medusa-improvement/src/lib.rs` and `crates/medusa-improvement/Cargo.toml`.

**Interfaces:**

This task consumes the completed `medusa_context::refinement` lifecycle and `LearningAdmissionPolicy`. It produces `RefinementAuthorityStore::open`, `snapshot`, `propose`, `validate`, `record_evaluation`, `approve`, `activate`, `supersede`, `defer`, `suspend`, `rollback`, `reject`, `tombstone`, `select`, `export`, and `migration_status`. Every mutation accepts `expected_revision: u64` and returns `RefinementAuthoritySnapshot`.

```rust
pub struct ApprovalBinding {
    pub proposal_id: String,
    pub proposal_version: u64,
    pub proposal_digest: String,
    pub actor_class: ApprovalActorClass,
    pub decision_id: String,
    pub issued_at_unix_ms: i64,
    pub receipt_digest: String,
}

pub struct SelectionContext {
    pub repository: Option<RepositoryIdentity>,
    pub user_id: String,
    pub session_id: Option<String>,
    pub task_kind: Option<String>,
    pub artifact_kind: Option<String>,
    pub context_tags: BTreeSet<String>,
    pub explicit_exclusions: BTreeSet<String>,
    pub objective: String,
    pub now_unix_ms: i64,
}
```

- [ ] **Step 1: Write failing durability and approval tests.** Tests must prove serialized approvals are untrusted until rebound to exact proposal content, stale revisions write nothing, activation is not visible when projection replacement is injected to fail, restart rebuilds the same active projection, corrupt projection rebuilds, corrupt authority fails closed and is copied to quarantine, and concurrent activation cannot publish two successors.

- [ ] **Step 2: Run the new test target and confirm RED.** Run `cargo test -p medusa-improvement --test refinement_authority -- --nocapture`. Expected failure is the missing `refinement_authority` module and types.

- [ ] **Step 3: Implement cross-platform persistence.** Use `.medusa/refinement-authority/journal.json`, `approvals.json`, `active.json`, `transactions`, and `quarantine`. Follow the repository's create-new lock, same-directory temporary file, `sync_all`, Windows-safe replacement, and idempotent transaction reconciliation patterns. Never trust `active.json` without matching its journal head hash and revision.

- [ ] **Step 4: Implement authority transitions and selection.** Bind approval digests to canonical serialized proposal content. Revalidate all approvals on open and immediately before activation. Materialize and verify the candidate projection before exposing it. Deterministically suppress nonmatching scopes and fail closed on equal-precedence conflicts.

- [ ] **Step 5: Run focused validation.** Run `cargo fmt --all -- --check`, `cargo test -p medusa-improvement --test refinement_authority`, and `cargo test -p medusa-improvement`.

- [ ] **Step 6: Commit the canonical store.** Stage the new authority, persistence, Cargo, module, and test files and commit with `feat(improvement): add durable refinement authority`.

### Task 3: Migrate every legacy learning authority without inferred trust

**Files:**

Create `crates/medusa-improvement/src/refinement_migration.rs` and `crates/medusa-improvement/tests/refinement_migration.rs`. Add fixtures under `crates/medusa-improvement/tests/fixtures/refinement-migration`. Modify `crates/medusa-improvement/src/learning_review.rs`, `crates/medusa-improvement/src/scoped_memory.rs`, and `crates/medusa-improvement/src/lib.rs`.

**Interfaces:**

This task consumes `RefinementAuthorityStore` and the current legacy schemas. It produces `RefinementMigrator::run(repo, store) -> Result<MigrationReport, RefinementAuthorityError>`, stable `MigrationReceipt`, `MigrationDisposition::{Imported, CompatibilityOnly, Quarantined, AlreadyImported}`, and read-only `LearningReviewSnapshot` compatibility projection from canonical state.

- [ ] **Step 1: Write one failing fixture test per legacy source.** Cover `.medusa/learning-review`, repository and user `learnings.json`, memory lessons, skill proposals and active skills with receipts, `ImprovementStore`, and `.medusa/engineering/improvements.json`. Assert that legacy `active`, `adopted`, or `approved` strings without a verifiable receipt import as non-active candidates. Assert idempotent reruns and bounded quarantine metadata for corrupt input.

```rust
assert_eq!(report.receipts[0].disposition, MigrationDisposition::Imported);
assert!(store.snapshot()?.active.is_empty());
assert_eq!(second_run.receipts[0].disposition, MigrationDisposition::AlreadyImported);
```

- [ ] **Step 2: Run the migration target and confirm RED.** Run `cargo test -p medusa-improvement --test refinement_migration -- --nocapture` and verify it fails because migration APIs do not exist.

- [ ] **Step 3: Implement deterministic adapters.** Fingerprint source path plus canonical source bytes, preserve stable IDs, versions, timestamps, scope, confidence, evidence, review decisions, hashes, receipts, and rollback lineage when present, and never synthesize missing approval or evaluation. Apply #818 admission and redaction before writing canonical content.

- [ ] **Step 4: Convert legacy stores to compatibility boundaries.** Keep legacy reads available during the bounded window. Route supported writes through the canonical authority; return `UnsupportedLegacyMutation` for states that cannot be translated without loss. Remove direct production activation from `LearningReviewStore` and `ScopedMemoryStore`.

- [ ] **Step 5: Run migration and existing compatibility suites.** Run `cargo test -p medusa-improvement --test refinement_migration`, `cargo test -p medusa-improvement learning_review`, and `cargo test -p medusa-improvement scoped_memory`.

- [ ] **Step 6: Commit the migration layer.** Stage migration code, fixtures, and legacy adapters and commit with `feat(improvement): migrate legacy learning authorities`.

### Task 4: Route commands and real turn context through the canonical authority

**Files:**

Create `crates/medusa-runtime/src/learning_authority.rs` and `crates/medusa-runtime/tests/refinement_runtime.rs`. Modify `crates/medusa-runtime/src/lib_wrapper.rs`, `crates/medusa-runtime/src/lib.rs`, `crates/medusa-runtime/src/commands.rs`, `crates/medusa-runtime/src/learning_retrieval.rs`, and `crates/medusa-runtime/src/learning_review.rs`.

**Interfaces:**

This task consumes the canonical store and migration layer. It produces frontend-neutral `learning_authority::read`, `transition`, `propose`, `record_evaluation`, `update_privacy`, `redaction_preview`, `export`, and `select`. It extends `LearningCommand` with `Inspect`, `Propose`, and `Evaluate` while preserving existing commands.

- [ ] **Step 1: Write failing runtime tests.** Through real `RuntimeController` or the narrowest production `run_prompt` harness, create a repository-scoped correction proposal without manual file editing, validate/evaluate/approve/activate it through shared commands, then prove a later matching turn includes its canonical ID/version/content and a nearby nonmatching turn does not. Add restart, suspend, rollback, corrupt projection recovery, corrupt authority fail-closed, and selection audit assertions.

- [ ] **Step 2: Write command parser contract tests.** Prove `/learning inspect ID`, `/learning propose repository KEY VALUE`, and `/learning evaluate ID pass` parse deterministically, reject missing arguments, and preserve value text after the fixed positional fields.

- [ ] **Step 3: Run focused runtime tests and confirm RED.** Run `cargo test -p medusa-runtime --test refinement_runtime -- --nocapture` plus the command parser unit test. Expected failures are missing canonical service and command variants.

- [ ] **Step 4: Implement the runtime service and command routing.** Run migration at runtime state load. Refresh and revalidate selection before every relevant turn. Replace direct `ScopedMemoryStore` retrieval and direct `LearningReviewStore` transitions. Emit truthful notices for selected, blocked, conflicted, quarantined, and unavailable state. Include proposal ID, version, scope, evidence IDs, approval receipt ID, and journal head hash in prompt provenance and selection audit.

- [ ] **Step 5: Preserve compatibility for existing frontends.** Make `medusa_runtime::learning_review` a canonical adapter returning the existing snapshot shape. Ensure TUI, desktop, daemon, and Telegram commands continue through the same runtime boundary.

- [ ] **Step 6: Run focused runtime validation.** Run `cargo fmt --all -- --check`, `cargo test -p medusa-runtime --test refinement_runtime`, `cargo test -p medusa-runtime`, and `cargo test -p medusa-tui`.

- [ ] **Step 7: Commit runtime activation.** Stage runtime source and tests and commit with `feat(runtime): apply canonical refinements to turns`.

### Task 5: Converge source writers and frontend projections

**Files:**

Modify `crates/medusa-agent/src/session/completed_learning.rs`, `crates/medusa-agent/src/session/lessons.rs`, `apps/medusa-desktop/src-tauri/src/learning.rs`, `apps/medusa-desktop/src-tauri/src/engineering.rs`, `apps/medusa-desktop/src/learningApi.ts`, and only the directly affected frontend tests. Create `crates/medusa-testkit/src/refinement_authority_policy.rs` and modify `crates/medusa-testkit/src/lib.rs` and `.github/workflows/data-lifecycle-certification.yml`.

**Interfaces:**

This task consumes runtime canonical operations. It produces canonical proposal admission for completed-session lessons, one canonical snapshot revision across frontends, read-only/untrusted engineering compatibility state, and a machine-enforced policy that rejects new frontend-owned writable improvement authorities.

- [ ] **Step 1: Write failing source-writer and parity tests.** Prove an authoritatively verified completed session creates a canonical proposal; a worker-local or unverified session creates none; arbitrary engineering text containing failure words creates no typed refinement; and desktop/TUI/runtime views report the same canonical proposal version and lifecycle.

- [ ] **Step 2: Write the architecture policy test.** Scan shipped source for forbidden direct writes to `.medusa/learning-review`, `.medusa/learnings.json`, and `.medusa/engineering/improvements.json`, allowing only migration adapters and fixtures. Assert `medusa-context` contains no filesystem I/O.

- [ ] **Step 3: Run focused tests and confirm RED.** Run `cargo test -p medusa-agent completed_learning -- --nocapture`, `cargo test --manifest-path apps/medusa-desktop/src-tauri/Cargo.toml learning -- --nocapture`, and `cargo test -p medusa-testkit refinement_authority_policy -- --nocapture`.

- [ ] **Step 4: Route source writers and frontends.** Admit completed lessons through the canonical proposal API. Make desktop learning commands use runtime canonical operations. Disable engineering lifecycle writes and expose imported legacy records as untrusted compatibility candidates until #824 replaces the dashboard.

- [ ] **Step 5: Add the policy gate to lifecycle CI.** Extend the existing cross-platform lifecycle certification workflow so changes to improvement, runtime, agent learning, or desktop engineering state run the authority policy and focused canonical tests.

- [ ] **Step 6: Run affected backend and frontend checks.** Run `cargo test -p medusa-agent completed_learning`, `cargo test -p medusa-testkit refinement_authority_policy`, `cargo test --manifest-path apps/medusa-desktop/src-tauri/Cargo.toml learning`, and from `apps/medusa-desktop`, run `npm.cmd test -- --run`, `npm.cmd run typecheck`, and `npm.cmd run build`.

- [ ] **Step 7: Commit convergence and policy.** Stage only source-writer, frontend adapter, testkit, workflow, and directly affected frontend files and commit with `feat: converge learned behavior on canonical authority`.

### Task 6: Complete documentation, full verification, PR, CI batch repair, and merge

**Files:**

Modify `docs/superpowers/specs/2026-08-12-unified-refinement-authority-design.md` and `docs/data-lifecycle-certification.md`. Create `docs/refinement-authority-migration.md` with legacy path dispositions, automatic migration behavior, command changes, quarantine recovery, and the compatibility removal dependencies.

**Interfaces:**

This task consumes the implemented behavior and produces current documentation, completion evidence, the single issue #819 PR, green CI, and a verified merge to `main`.

- [ ] **Step 1: Update documentation to actual behavior.** Record canonical paths, ownership, command syntax, migration dispositions, compatibility window, privacy behavior, rollback, quarantine, and removal dependencies on #760 and #824. Remove statements contradicted by the final code.

- [ ] **Step 2: Run focused checks again.** Run `cargo fmt --all -- --check`, `cargo test -p medusa-context`, `cargo test -p medusa-improvement`, `cargo test -p medusa-runtime`, `cargo test -p medusa-agent completed_learning`, and `cargo test -p medusa-testkit refinement_authority_policy`.

- [ ] **Step 3: Run workspace verification.** Run `cargo clippy --workspace --all-targets --all-features -- -D warnings` and `cargo test --workspace --all-targets --all-features`. Record exact failures without weakening tests, fixtures, policy, or expected outputs.

- [ ] **Step 4: Audit requirements against current evidence.** Re-read issue #819 and the approved design. For every required lifecycle, migration source, frontend, recovery case, approval boundary, selection behavior, and completion rule, point to a passing test or current-state artifact. Treat missing proof as unfinished implementation.

- [ ] **Step 5: Commit final documentation and verification fixes.** Stage the scoped files, run `git diff --cached --check` plus a secret-pattern scan, and commit with `docs: document unified refinement authority` or a more specific fix message.

- [ ] **Step 6: Push and open one ready PR for issue #819.** Push `codex/issue-819-refinement-authority`, create the PR against `main`, include exact local validation and `Closes #819`, and verify the remote head SHA.

- [ ] **Step 7: Collect the complete first CI result before editing.** Wait until every applicable check reaches a terminal state. Fetch all failed job logs and all review threads, cluster the full failure set, and do not restart or push while another applicable job is still producing potentially actionable evidence.

- [ ] **Step 8: Fix the entire CI/review set in one batch.** Reproduce each failure locally where possible, add or adjust regression coverage without hiding product defects, run the union of affected checks, commit once, and push once. Then wait for the restarted CI.

- [ ] **Step 9: Merge only after authoritative green evidence.** Verify every applicable check is passing, no unresolved actionable review thread remains, the PR is mergeable and current with `main`, then merge on GitHub and delete the remote feature branch. Verify PR merged state, issue #819 closure, and `refs/heads/main` at the merge SHA.
