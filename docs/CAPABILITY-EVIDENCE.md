# Medusa Capability Evidence

Status snapshot: **July 20, 2026**, based on `main` through merged PR #85. This document is an evidence ledger, not a promise that every long-term product goal is complete.

## Evidence rules

A capability is listed as **shipped** only when its production code is on `main` and covered by the repository's normal validation. Open pull requests, temporary writer workflows, branch-only diagnostics, and design intentions do not count as shipped.

When a production boundary moves, its behavior tests must move with it or be replaced at the correct layer; orphaned test files and coverage percentages alone are not completion evidence.

The authoritative order is:

1. production code and tests on `main`;
2. required GitHub Actions gates;
3. merged pull-request history;
4. this evidence summary;
5. historical phase plans.

## Shipped on `main`

| Capability | Production evidence | Gate evidence |
|---|---|---|
| CLI and interactive entry point | `crates/medusa-cli`, `crates/medusa-tui` | Workspace build, Clippy, tests, docs, and package smoke jobs |
| Full conversation transcript and distinct user/assistant presentation | `crates/medusa-tui` | TUI tests in the workspace suite |
| Markdown rendering and mid-turn follow-up queueing | `crates/medusa-runtime`, `crates/medusa-tui`; introduced in PR #34 and moved behind the shared runtime in PR #39 | Runtime behavior tests, TUI mapping tests, workspace tests, and source-size guardrail |
| Clipboard text and screenshot prompts | Frontend-neutral prompt types in `crates/medusa-runtime`; OS clipboard access in `crates/medusa-tui` | Runtime/TUI tests and cross-platform package smoke |
| Shared frontend-neutral interactive runtime | `crates/medusa-runtime`; extracted in PR #39, with `crates/medusa-tui` reduced to a terminal adapter | Runtime behavior tests, TUI mapping tests, workspace Clippy/tests, coverage, and package smoke |
| Zeus-derived desktop entry point | `apps/medusa-desktop`; React/Tauri shell connected directly to `medusa-runtime` | Desktop frontend tests/build plus cross-platform Rust adapter Clippy/tests |
| Validated desktop bundles | Tauri DEB/AppImage, app/DMG, and NSIS targets normalized and validated by PR #63 | Three-platform `Desktop` bundle matrix, version synchronization, minimum-size/path checks, and SHA-256 manifests |
| Agent loop, planning, cancellation, tools, and verification | `crates/medusa-agent`, `crates/medusa-protocol`, `crates/medusa-provider` | Workspace tests plus named adversarial regressions |
| Repository parsing, patching, and guarded transactions | `crates/medusa-intelligence`, `crates/medusa-agent` | Patch-transaction regression and workspace tests |
| Durable Markdown memory and lifecycle controls | `crates/medusa-memory` | Workspace tests and migration/rollback checks |
| Parallel workers and deterministic merge handling | `crates/medusa-workers` | Parallel merge and conflict-abort regressions |
| Browser verification sidecar | `crates/medusa-browser-client`, `crates/medusa-browserd` | Workspace tests and package validation |
| Skills, hooks, and MCP isolation | `crates/medusa-extensions` | Malicious-MCP regression and workspace tests |
| Desktop Commander MCP integration | `crates/medusa-extensions`; merged in PR #37 with lockfile follow-up in PR #38 | MCP tests, dependency policy, and canonical CI |
| Cross-platform persistent daemon | `crates/medusa-daemon`; transport/recovery in PR #47, frontend lifecycle supervision in PR #54, and process-tree cancellation in PR #59 | `Daemon` and `Desktop` matrices on Ubuntu, macOS, and Windows; reconnect, startup-race, load, queue, cancellation, and shutdown tests |
| Bounded daemon concurrency | Four fixed workers, 32 queued jobs, `daemon_busy`, finite IPC timeouts, 64 KiB request cap | 64-client burst, exact one-worker/one-queue backpressure, graceful drain evidence on all three platforms |
| Race-safe daemon cancellation and forced shutdown | Additive `Cancel` and `ShutdownNow` requests, per-job process controls, Unix process groups, Windows task-tree termination, durable `interrupted` records; Windows process containment strengthened in PR #81 | Queued work never executes; descendants terminate within a bound; unrelated processes remain alive; immediate shutdown is bounded on all three platforms |
| Operational hardening, migrations, archives, redaction, and recovery | `crates/medusa-hardening` | Release Gates: coverage, adversarial regressions, fuzz, chaos, security, and package smoke |
| Production panic and workflow hygiene | Panic-free production Clippy target and read-only workflow guardrails from PRs #44–#45 | CI panic audit, source-size ceiling, and workflow-hygiene checks |
| Dependency hygiene | Direct dependency pruning and permanent graph metrics from PR #52 | Base/current dependency policy, cargo-deny, and cargo-audit |
| Deterministic release evidence | `scripts/release-evidence.py` generates synchronized-version checks, a CycloneDX 1.6 SBOM, complete asset manifests, and `SHA256SUMS`; merged in PR #67 | Fixture adversarial tests, real Cargo/npm lockfile SBOM generation, YAML parsing, documentation gates, and three-platform desktop packaging |
| Attested draft release publication | `.github/workflows/publish-release.yml` accepts only a pushed version tag bound to the workflow event SHA, unchanged remote tag target, and `main` ancestry; it builds all platform assets, uses `actions/attest@v4`, and creates a draft release | Least-privilege workflow guard, exact-head CI/Daemon/Desktop/guardrails, full Release Gates, and explicit refusal to auto-publish or overwrite an existing release |
| Provider configuration and resilience | Runtime provider configuration is wired into execution in PR #72 and managed through the resilient provider manager introduced in PR #77 | Provider configuration, failover behavior, workspace tests, and live-provider gates |
| Verified self-update | PRs #73, #76, #78, #82, and #83 add immutable-main resolution, configured update policy, detached replacement, Windows process shutdown/restart handling, and repositories-without-releases behavior | CLI/update tests, cross-platform package validation, and canonical CI |
| First-class GitHub service | `crates/medusa-github` and runtime integration introduced in PR #74 | GitHub service unit/integration tests and workspace validation |
| Shared runtime capability registry | PR #75 centralizes runtime capability reporting so frontends and prompts consume one authoritative capability matrix | Registry tests, runtime tests, and workspace validation |
| Shared tool manager | PR #79 introduces one manager boundary for tool discovery, policy, and execution wiring | Tool manager tests, workspace Clippy/tests, and source-size guardrail |
| Modular manager architecture | Manager boundaries and ownership are documented in PR #80 | Documentation checks and architecture consistency review |
| Independent Medusa identity grounding | PR #85 makes Medusa identity and runtime capabilities authoritative and removes automatic `CLAUDE.md` instruction loading | Focused prompt/instruction tests plus canonical CI and Refactor Guardrails; PR #87 adds adversarial repository-configuration regressions |

## Canonical gates

- **CI** runs the complete workspace quality suite, production panic audit, documentation, dependency policy, deterministic release-evidence tests, real-lockfile SBOM generation, and static parsing of the tag-only release workflow. Its concurrency group cancels superseded runs for the same ref.
- **Daemon** runs daemon/TUI formatting, Clippy, reconnect/recovery, lifecycle, load, queue, cancellation, and shutdown tests on Ubuntu, macOS, and Windows.
- **Desktop** validates the React frontend, shared Tauri/runtime/daemon adapter, release-evidence fixtures, and unsigned DEB/AppImage, app/DMG, and NSIS bundles on all three platforms. Changes to release packaging logic trigger this matrix.
- **Refactor Guardrails** enforces the 800-line production source ceiling, baseline documents, and workflow hygiene. The sole release writer is explicitly registered and cannot push commits or publish a release.
- **Release Gates** runs coverage, adversarial, fuzz, chaos, cross-platform packaging, documentation/schema, security, and live-provider jobs. Draft pull requests skip these expensive jobs; marking a pull request ready activates them.
- **Publish Draft Release** is intentionally not a pull-request gate. It runs only after a version tag is pushed, revalidates tag immutability and `main` ancestry, and creates a draft with deterministic evidence and short-lived OIDC provenance.

Skipping expensive release jobs on drafts changes scheduling, not acceptance criteria: merge readiness still requires the full configured gate set.

## Current architecture boundary

Issue #42 is completed: production panic paths, Windows daemon parity, bounded concurrency, workflow hygiene, dependency pruning, and shared frontend lifecycle ownership are all merged with evidence.

Issue #56 is completed in PR #59: daemon jobs support race-safe per-job cancellation and bounded immediate process-tree shutdown while retaining graceful drain semantics and rollback-readable durable state.

Issue #66 is completed in PR #67: release packaging has a permanent tag-bound draft workflow, deterministic SBOM/checksum evidence, three-platform assets, and GitHub/Sigstore provenance without automatic publication.

PRs #72–#85 extend the production architecture with provider configuration, verified updates, a first-class GitHub service, a shared capability registry, resilient provider and tool managers, stronger Windows containment, and independent Medusa identity grounding.

Issue #86 tracks the remaining product-hardening program. The next product boundary is desktop parity: session discovery, richer diffs, structured approvals, memory browsing, accessibility, and a complete repository-to-PR flow. This work must continue using the shared `medusa-runtime`, provider stack, capability registry, tool manager, and daemon contract.

Remaining release trust work requires external platform credentials and custody policy: Windows Authenticode, macOS Developer ID signing/notarization, and signed Linux distribution channels. Provenance attestations establish build origin and integrity but do not replace operating-system trust.

## Documentation policy

`README.md` is the product overview and installation guide. This file is the status/evidence ledger. Historical phase documents may explain intent, but they must not claim completion that is contradicted by `main`, open pull requests, or required checks. Every merged capability PR must update this ledger or include an explicit reason why no evidence entry changes. A new completion snapshot should update this ledger instead of creating another competing `FINAL.md`.
