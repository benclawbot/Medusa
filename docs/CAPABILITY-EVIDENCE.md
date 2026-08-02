# Medusa Capability Evidence

This document is the durable evidence ledger for capabilities represented on `main`. The machine-readable authority is [`CAPABILITY-CLAIMS.json`](CAPABILITY-CLAIMS.json), validated by `scripts/check-capability-evidence.py`.

## Evidence rules

A capability is represented by exactly one maturity value:

- `production`: available through supported production entrypoints, enabled by default where applicable, and backed by an owner, tests, supported platforms, observability, documentation, and canonical gates.
- `preview`: usable only through an explicit opt-in; compatibility and support may still change.
- `experimental`: research or early implementation that requires a deliberate feature or configuration opt-in.
- `design-only`: architecture or scaffolding with no production entrypoint or opt-in. Production capabilities may not depend on it.

Production code and tests on `main` remain the highest authority. Every capability-changing pull request must update the manifest and ledger or explain why no capability record changes.

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

The manifest also records production paths, behavioral test paths, canonical gates, observability references, public documentation, promotion evidence, default activation, explicit opt-ins, and capability dependencies.

## Production capability evidence

- `shared-runtime`: `crates/medusa-runtime`, `crates/medusa-tui`, and `apps/medusa-desktop`; validated by CI, Desktop, and Refactor Guardrails.
- `durable-sessions-memory`: session persistence and `crates/medusa-memory`; validated by CI and Release Gates.
- `github-service`: `crates/medusa-github`, the approval-gated `medusa-capabilities create-repository` entrypoint, and the backend-neutral `medusa-github-operation` entrypoint. Repository management uses one serialized, repository-confined operation contract with interchangeable native GitHub CLI and `gh api` REST transports, normalized receipts, bounded and redacted payloads, exact ordinary and high-risk approvals, and durable audit evidence. Existing typed convenience methods and repository creation retain their specialized lifecycle behavior. Validated by CI and Release Gates; documented in [`explicit-capabilities.md`](explicit-capabilities.md), [`GITHUB-REPOSITORY-CREATION.md`](GITHUB-REPOSITORY-CREATION.md), and [`GITHUB-BACKEND-PARITY.md`](GITHUB-BACKEND-PARITY.md).
- `provider-context-resilience`: provider, runtime, and agent layers; validated by CI and Release Gates.
- `identity-approval-transactions`: identity guard, approval, transaction, and engine wiring with named safety tests; validated by CI, Release Gates, and Refactor Guardrails.
- `daemon`: `crates/medusa-daemon`; validated by Daemon, Desktop, and CI.
- `release-trust`: release evidence scripts and publish workflow; validated by CI, Desktop, Release Gates, and Refactor Guardrails.
- `self-update`: `crates/medusa-cli`; validated by CI, Desktop, and Release Gates.
- `multi-agent-research`: `run_prompt` dispatches independent read-only planner and risk-reviewer `AgentEngine` sessions under durable leases. Explicit mutation objectives then run an implementer `AgentEngine` in an execution-specific isolated worktree, reject out-of-scope or overlapping changes, verify before integration, roll back integration conflicts, clean temporary resources, and hand durable evidence to a read-only parent reviewer. Validated by CI, Daemon, Desktop, and Refactor Guardrails.

## Planned and scaffolding behavior

### Remaining design-only boundary

The current production capability supports one mutating implementer contract after parallel read-only preflight. Autonomous nested delegation, dynamic multi-implementer task creation, consensus voting, commit barriers, and distributed transaction coordination remain design-only until a production caller, recovery path, observability contract, and behavioral proof are merged. Their presence in the workspace must not be presented as active behavior.

## Canonical gates

- **CI** validates formatting, Clippy, panic-free production targets, workspace tests, documentation, dependency policy, release-evidence fixtures, SBOM generation, and workflow parsing.
- **Daemon** validates daemon lifecycle behavior on Linux, macOS, and Windows.
- **Desktop** validates the React/Tauri frontend, shared runtime adapter, daemon integration, and unsigned cross-platform bundles.
- **Refactor Guardrails** enforces workflow permissions, architecture metadata, and this maturity contract.
- **Release Gates** validates coverage, adversarial regressions, fuzzing, chaos recovery, security, packages, documentation/schema consistency, and live-provider scenarios.

## Operational boundaries

Platform support is explicit per capability and does not imply identical containment internals. External dependencies are recorded so provider APIs, Git services, Node sidecars, or artifact-attestation infrastructure cannot be mistaken for repository-owned guarantees. README, configuration, compatibility, and release documentation may describe only behavior at or below the recorded maturity.
