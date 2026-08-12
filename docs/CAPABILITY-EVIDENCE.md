# Medusa Capability Evidence

This document is the durable evidence ledger for capabilities represented on `main`. The machine-readable legacy-availability authority is [`CAPABILITY-CLAIMS.json`](CAPABILITY-CLAIMS.json), validated by `scripts/check-capability-evidence.py`.

Architecture-v2 certification is separate and is governed by [`architecture/INDEX.md`](architecture/INDEX.md) and [`architecture/baseline.json`](architecture/baseline.json). A legacy `production` value means a supported current entrypoint exists; it does **not** certify the capability against the v2 authority, lifecycle, dispatcher, review, verification, provider, trust-boundary, or deletion requirements.

## Evidence rules

A legacy capability is represented by exactly one availability maturity:

- `production`: available through supported current entrypoints, enabled by default where applicable, and backed by the recorded owner, tests, platforms, observability, documentation, and canonical gates.
- `preview`: usable only through an explicit opt-in; compatibility and support may still change.
- `experimental`: research or early implementation that requires a deliberate feature or configuration opt-in.
- `design-only`: architecture or scaffolding with no production entrypoint or opt-in. Production capabilities may not depend on it.

Architecture v2 adds a separate certification status: `certified-production`, `legacy-uncertified`, `quarantined`, or `design-only`. Production code and executable tests remain the highest authority for current behavior. Every capability-changing pull request must update the applicable legacy claim, v2 inventory, index, tests, and deletion target or explain why no record changes.

## Capability maturity matrix

| Claim | Maturity | Owner | Production entrypoint | Platforms | External dependencies |
|---|---|---|---|---|---|
| `shared-runtime` | `production` | runtime maintainers | `medusa`, desktop app | Linux, macOS, Windows | none |
| `durable-sessions-memory` | `production` | agent runtime maintainers | `medusa`, `medusa run` | Linux, macOS, Windows | none |
| `github-service` | `production` | integration maintainers | guarded GitHub workflows, `medusa-capabilities create-repository`, `medusa-github-operation` | Linux, macOS, Windows | GitHub API, GitHub CLI, Git |
| `provider-context-resilience` | `production` | provider maintainers | `medusa`, `medusa run`, `medusa quickstart` | Linux, macOS, Windows | configured model provider |
| `identity-approval-transactions` | `production` | safety maintainers | `medusa`, `medusa run` | Linux, macOS, Windows | none |
| `daemon` | `production` | daemon maintainers | daemon and desktop adapter | Linux, macOS, Windows | none |
| `release-trust` | `production` | release maintainers | publish-release workflow | Linux, macOS, Windows | GitHub artifact attestations |
| `self-update` | `production` | CLI maintainers | `medusa update` | Linux, macOS, Windows | GitHub repository access |
| `multi-agent-research` | `production` | agent runtime maintainers | coordinated `run_prompt` preflight and worktree implementation | Linux, macOS, Windows | configured model provider, Git |
| `truthful-code-intelligence-levels` | `production` | code intelligence maintainers | `semantic_capabilities`, `code_index`, `typescript_semantic`, `symbol_rename` | Linux, macOS, Windows | `typescript-language-server` for TypeScript/JavaScript semantic operations |

The manifest records current production paths, behavioral test paths, canonical gates, observability references, public documentation, promotion evidence, default activation, explicit opt-ins, and capability dependencies. The v2 baseline additionally records dispositions, exact blockers, source-of-truth ownership, trust boundaries, migration consumers, and legacy deletion targets.

## Architecture v2 certification baseline

| Capability | Legacy availability | V2 certification | Blocking evidence |
|---|---|---|---|
| Shared runtime | production | legacy-uncertified | lifecycle and mutable authority remain implicit across runtime and projections |
| Durable sessions and memory | production | legacy-uncertified | multiple projections require one declared authority per concern |
| GitHub service | production | legacy-uncertified | migrate OAuth/backend operations behind the final versioned integration boundary |
| Provider/context resilience | production | quarantined | streaming, cancellation, fallback-health, and readiness claims do not fully match wire behavior |
| Identity/approval/transactions | production | legacy-uncertified | mutation authority and receipts remain split |
| Daemon | production | legacy-uncertified | daemon and remote contracts are not yet versioned |
| Release trust | production | legacy-uncertified | updater does not consume the verified prebuilt channel until #655 |
| Self-update | production | quarantined | the default path compiles from source and takes minutes |
| Multi-agent research | production | quarantined | integration precedes independent parent review; changed paths are not explicit verification input; task/reviewer state is partly decorative |
| Browser tools | advertised outside this legacy claim set | quarantined | production `execute_tool` dispatch is absent |
| Plugins/extensions | structural | design-only | no certified manifest, permissions, dispatcher, lifecycle, or durable result contract |
| Telegram remote frontend | partial | quarantined | shared-path and operator conformance are incomplete |
| Unsafe/FFI boundary | partial | legacy-uncertified | #653 owns the explicit unsafe allowlist and audit boundary |
| TypeScript/JavaScript code intelligence | production | certified-production | none; final issue-closing certification binds dispatcher, freshness, benchmarks, architecture, and cross-platform evidence |

These downgrades prevent current gaps from being presented as architecture-v2 guarantees while preserving truthful evidence about existing entrypoints.

## Production capability evidence

- `shared-runtime`: `crates/medusa-runtime`, `crates/medusa-tui`, and `apps/medusa-desktop`; validated by CI, Desktop, and Refactor Guardrails. V2 will replace implicit lifecycle ownership with versioned command, event, evidence, and artifact contracts.
- `durable-sessions-memory`: session persistence and `crates/medusa-memory`; validated by CI and Release Gates. UI and process-local projections are not independent authorities.
- `github-service`: `crates/medusa-github`, the approval-gated repository-creation entrypoint, and the backend-neutral operation entrypoint. Repository management uses serialized, repository-confined typed operations, normalized receipts, bounded and redacted payloads, approval tiers, and durable audit evidence. Validated by CI and Release Gates.
- `provider-context-resilience`: provider, runtime, and agent layers; validated by current CI and Release Gates for legacy availability. The production contract now also exposes validated role/phase route pins and a bounded provider-agnostic reasoning handoff, while opaque native continuation remains fail-closed until a reviewed adapter advertises exact wire semantics. V2 certification remains quarantined because configuration can claim streaming while requests force `stream=false`, cancellation can return while a blocking request thread remains active, and route health/readiness has competing projections.
- `identity-approval-transactions`: identity guard, approval, transaction, and engine wiring with named safety tests; validated by CI, Release Gates, and Refactor Guardrails. V2 must centralize mutation authority and receipts.
- `daemon`: `crates/medusa-daemon`; validated by Daemon, Desktop, and CI. Remote frontend certification remains separate.
- `release-trust`: release evidence scripts and publish workflows; validated by CI, Desktop, Release Gates, and Refactor Guardrails. #655 connects immutable Ed25519-verified prebuilt artifacts to the updater without requiring paid platform signing.
- `self-update`: current CLI entrypoint in `crates/medusa-cli` and `crates/medusa-update`; available on supported platforms, but quarantined for v2 because the default update path compiles from source.
- `multi-agent-research`: `run_prompt` dispatches independent read-only planner and risk-reviewer `AgentEngine` sessions under durable leases. Explicit mutation objectives then run an implementer `AgentEngine` in an execution-specific isolated worktree, reject out-of-scope or overlapping changes, verify the worktree, prepare a commit, integrate it, and only then hand evidence to the read-only parent reviewer. The current manager can roll back integration conflicts, but the review-after-integration order and changed-path verification gap are known failures, not v2 guarantees. Validated as legacy availability by CI, Daemon, Desktop, and Refactor Guardrails.
- `truthful-code-intelligence-levels`: `semantic_capabilities` reports exact Rust, Python, and TypeScript/JavaScript depth. `typescript_semantic` dispatches repository-scoped definitions, references, diagnostics, and workspace symbols. `symbol_rename` routes TypeScript/JavaScript through an exact workspace symbol, `prepareRename`, independent references, normalized workspace-edit validation, deterministic repository/workspace fingerprints, exact touched-file snapshots, and the guarded `PatchTransaction`. Monorepo, ignored/generated, repository-switching, stale-state, Unicode cross-file, large-workspace, benchmark, and cross-platform certification evidence are retained in the code-intelligence architecture record and final gate.

## Planned and scaffolding behavior

### Remaining design-only boundary

The current coordinated path supports one mutating implementer contract after parallel read-only preflight. Autonomous nested delegation, dynamic multi-implementer task creation, consensus voting, commit barriers, and distributed transaction coordination remain design-only until a production caller, one durable state authority, recovery path, observability contract, permissions, and behavioral proof are merged.

Browser and plugin structure must not be presented as active capability merely because crates, schemas, or tool definitions exist. Architecture v2 requires definition → readiness → permission → dispatch → side effect → evidence → event delivery → cleanup conformance.

## Canonical gates

- **CI** validates formatting, Clippy, panic-free production targets, workspace tests, documentation, dependency policy, release-evidence fixtures, SBOM generation, and workflow parsing.
- **Daemon** validates daemon lifecycle behavior on Linux, macOS, and Windows.
- **Desktop** validates the React/Tauri frontend, shared runtime adapter, daemon integration, and unsigned cross-platform bundles.
- **Refactor Guardrails** enforces workflow permissions, current architecture metadata, and legacy maturity contracts.
- **Architecture v2 Baseline** validates the living index, workspace/component inventory, production paths, duplicate authorities, forbidden dependencies, PR governance, CODEOWNERS, real CLI entrypoints, and removable expected-failure fixtures on Linux, macOS, and Windows.
- **Release Gates** validates coverage, adversarial regressions, fuzzing, chaos recovery, security, packages, documentation/schema consistency, and live-provider scenarios.
- **Code Intelligence Certification** installs the production TypeScript language server and validates formatting, linting, correctness/freshness fixtures, production agent tests, benchmark compilation/execution, and architecture ownership on Linux, macOS, and Windows for the final issue-closing PR.

## Operational boundaries

Platform support is explicit per capability and does not imply identical containment internals. External dependencies are recorded so provider APIs, Git services, Node sidecars, or artifact-attestation infrastructure cannot be mistaken for repository-owned guarantees. README, configuration, compatibility, release documentation, and UI labels may describe only behavior at or below the recorded legacy availability and v2 certification.

- The `truthful-code-intelligence-levels` claim is recorded in the maturity matrix above and in `CAPABILITY-CLAIMS.json`. Its typed profiles, registry permissions, production dispatch, deterministic freshness evidence, guarded mutation path, architecture record, and benchmark must remain synchronized.
