# Medusa Capability Evidence

This document is the durable evidence ledger for capabilities shipped on `main`. It intentionally avoids dated pull-request states and transient workflow snapshots. The machine-readable source is [`CAPABILITY-CLAIMS.json`](CAPABILITY-CLAIMS.json), validated by `scripts/check-capability-evidence.py`.

## Evidence rules

A capability is **shipped** only when production code, behavioral tests, and named repository gates are represented in the checked-out commit. Branch-only diagnostics, temporary workflows, design documents, and historical completion statements do not count as shipped behavior.

The authoritative order is:

1. production code and tests on `main`;
2. canonical GitHub Actions definitions and retained evidence;
3. merged pull-request history;
4. `docs/CAPABILITY-CLAIMS.json`;
5. this human-readable ledger;
6. historical phase plans.

Every shipped claim must name existing production paths, existing test paths, and canonical gates. Repository guardrails reject deleted file references, unknown gates, volatile PR-state snapshots, unsupported passing-test claims, duplicate claim identifiers, and ledger/manifest drift.

## Shipped on `main`

| Claim | Capability | Production evidence | Gate evidence |
|---|---|---|---|
| `shared-runtime` | Shared TUI and desktop runtime | `crates/medusa-runtime`, `crates/medusa-tui`, `apps/medusa-desktop` | CI, Desktop, Refactor Guardrails |
| `durable-sessions-memory` | Durable sessions, prompts, Markdown memory, provenance, lifecycle state, and recall | `crates/medusa-agent/src/session.rs`, `crates/medusa-memory` | CI, Release Gates |
| `github-service` | Guarded GitHub authentication and repository workflow service | `crates/medusa-github` | CI, Release Gates |
| `provider-context-resilience` | Provider configuration, retry/failover, capability authority, context accounting, and compaction | `crates/medusa-provider`, `crates/medusa-runtime`, `crates/medusa-agent` | CI, Release Gates |
| `identity-approval-transactions` | Independent Medusa identity; exact-action, plan-bound, expiring approvals; durable decisions; atomic rollback evidence | `crates/medusa-agent/src/identity_guard.rs`, `approval.rs`, `transaction.rs`, and runtime wiring in `engine.rs` | CI, Release Gates, Refactor Guardrails |
| `daemon` | Cross-platform bounded daemon concurrency, reconnect, cancellation, process-tree termination, drain, and recovery | `crates/medusa-daemon` | Daemon, Desktop, CI |
| `release-trust` | Validated cross-platform artifacts, deterministic checksums/SBOM, provenance attestations, and draft-only publication | `scripts/release-evidence.py`, `.github/workflows/publish-release.yml` | CI, Desktop, Release Gates, Refactor Guardrails |
| `self-update` | Verified immutable-`main` update checks and replacement with package-manager ownership safeguards | `crates/medusa-cli` | CI, Desktop, Release Gates |

Additional shipped product boundaries remain covered by the workspace and the canonical gates:

- repository parsing, guarded filesystem access, search, patch transactions, shell policy, checkpoints, and targeted verification;
- transcript rendering, clipboard text and image prompts, queued guidance, plans, questions, cancellation, and resume;
- desktop session discovery, multi-file diffs, structured approvals, memory browsing, accessibility, and repository-to-PR flows through the shared runtime;
- browser verification through the Playwright sidecar;
- parallel workers with isolated worktrees and deterministic conflict handling;
- skills, hooks, MCP isolation, optional Desktop Commander integration, provenance, and redaction;
- source-size, panic, dependency, migration, rollback, fuzz, chaos, security, packaging, and live-provider hardening.

## Canonical gates

- **CI** validates formatting, Clippy, panic-free production targets, complete workspace tests, documentation, dependency policy, release-evidence fixtures, SBOM generation, and workflow parsing.
- **Daemon** validates daemon and frontend lifecycle behavior on Linux, macOS, and Windows, including reconnect, overload, cancellation, shutdown, and recovery.
- **Desktop** validates the React/Tauri frontend, shared runtime adapter, daemon integration, and unsigned Linux, macOS, and Windows bundles.
- **Refactor Guardrails** enforces source-size ceilings, public baselines, workflow permissions, and capability-evidence synchronization.
- **Release Gates** validates coverage, named adversarial regressions, fuzzing, chaos recovery, security, packages, documentation/schema consistency, and live-provider scenarios.

A gate name in the claims manifest must match this retained set. Draft scheduling may defer expensive work, but it does not change acceptance criteria.

## Operational boundaries

### GitHub authentication and permissions

Medusa discovers the authenticated GitHub account through the configured GitHub service and reports missing or expired authentication as a recoverable configuration error. Read operations require repository visibility; externally visible or destructive operations require structured confirmation. Dirty-worktree and force-operation protections remain authoritative even when authentication permits the underlying API call.

### Provider retry, failover, and context

Provider selection, retry and failover decisions are centralized in the provider/runtime layer. Frontends consume runtime events and do not invent provider capabilities. Context accounting and compaction preserve the active objective, plan, durable evidence, provenance, and recent interaction boundaries rather than silently discarding state.

### Large repositories and indexing

Repository discovery and indexing are bounded. Medusa prefers targeted search, repository maps, and incremental context rather than attempting to load an entire large repository into a model request. Limits and exclusions are surfaced as runtime evidence instead of being represented as complete coverage.

### Installation, upgrades, and rollback

The CLI may be installed from the repository with Cargo or through validated release assets. `medusa update --check` is read-only. Automatic replacement is policy-controlled, resolves an immutable `main` commit, respects package-manager ownership, and uses detached replacement where required. Release assets remain draft-only until maintainer review; operating-system signing and notarization require external credential custody and are not implied by provenance attestations.

## Documentation policy

`README.md` is the product overview and installation guide. This ledger is the human-readable status record. `CAPABILITY-CLAIMS.json` is the synchronization contract used by CI. Historical documents may explain intent, but they must not contradict current production paths, tests, architecture, or gates. Every capability-changing pull request must update the manifest and ledger or state why no shipped claim changes.
