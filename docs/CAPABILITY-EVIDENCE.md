# Medusa Capability Evidence

This document is the durable evidence ledger for capabilities represented on `main`. The machine-readable authority is [`CAPABILITY-CLAIMS.json`](CAPABILITY-CLAIMS.json), validated by `scripts/check-capability-evidence.py`.

## Evidence rules

A capability is represented by exactly one maturity value:

- `production`: available through supported production entrypoints, enabled by default where applicable, and backed by an owner, tests, supported platforms, observability, documentation, and canonical gates.
- `preview`: usable only through an explicit opt-in; compatibility and support may still change.
- `experimental`: research or early implementation that requires a deliberate feature or configuration opt-in.
- `design-only`: architecture or scaffolding with no production entrypoint or opt-in. Production capabilities may not depend on it.

Production code and tests on `main` remain the highest authority. The manifest records that authority in reviewable metadata; this ledger renders the public status without inventing stronger claims.

The authoritative order is:

1. production code and tests on `main`;
2. canonical GitHub Actions definitions and retained evidence;
3. `docs/CAPABILITY-CLAIMS.json`;
4. this human-readable ledger;
5. README and release claims;
6. historical design documents.

Every capability-changing pull request must update the manifest and ledger or explain why no capability record changes. Guardrails reject deleted file references, unknown gates or platforms, missing production evidence, incomplete promotion checklists, silent non-production defaults, design-only entrypoints, and production dependencies on non-production capabilities.

## Capability maturity matrix

| Claim | Maturity | Owner | Production entrypoint | Platforms | External dependencies |
|---|---|---|---|---|---|
| `shared-runtime` | `production` | runtime maintainers | `medusa`, desktop app | Linux, macOS, Windows | none |
| `durable-sessions-memory` | `production` | agent runtime maintainers | `medusa`, `medusa run` | Linux, macOS, Windows | none |
| `github-service` | `production` | integration maintainers | guarded GitHub workflows | Linux, macOS, Windows | GitHub API |
| `provider-context-resilience` | `production` | provider maintainers | `medusa`, `medusa run`, `medusa quickstart` | Linux, macOS, Windows | configured model provider |
| `identity-approval-transactions` | `production` | safety maintainers | `medusa`, `medusa run` | Linux, macOS, Windows | none |
| `daemon` | `production` | daemon maintainers | daemon and desktop adapter | Linux, macOS, Windows | none |
| `release-trust` | `production` | release maintainers | publish-release workflow | Linux, macOS, Windows | GitHub artifact attestations |
| `self-update` | `production` | CLI maintainers | `medusa update` | Linux, macOS, Windows | GitHub repository access |
| `multi-agent-research` | `design-only` | agent research maintainers | none | repository scaffolding is cross-platform | none |

The manifest also records production paths, behavioral test paths, canonical gates, observability references, public documentation, promotion evidence, default activation, explicit opt-ins, and capability dependencies.

## Production capability evidence

- `shared-runtime`: `crates/medusa-runtime`, `crates/medusa-tui`, and `apps/medusa-desktop`; validated by CI, Desktop, and Refactor Guardrails.
- `durable-sessions-memory`: session persistence and `crates/medusa-memory`; validated by CI and Release Gates.
- `github-service`: `crates/medusa-github`; validated by CI and Release Gates.
- `provider-context-resilience`: provider, runtime, and agent layers; validated by CI and Release Gates.
- `identity-approval-transactions`: identity guard, approval, transaction, and engine wiring with named safety tests; validated by CI, Release Gates, and Refactor Guardrails.
- `daemon`: `crates/medusa-daemon`; validated by Daemon, Desktop, and CI.
- `release-trust`: release evidence scripts and publish workflow; validated by CI, Desktop, Release Gates, and Refactor Guardrails.
- `self-update`: `crates/medusa-cli`; validated by CI, Desktop, and Release Gates.

## Planned and scaffolding behavior

### Design-only boundary

`multi-agent-research` covers retained scheduler, worker-lease, isolated-worker transaction, consensus, and commit-barrier concepts. The production runtime uses one `AgentEngine` and does not dispatch workers or subagents. This capability has no production entrypoint, no runtime opt-in, is disabled by definition, and cannot be a dependency of a `production` record.

This boundary preserves the production multi-agent work already integrated into the runtime contracts without claiming that remaining research scaffolding is active worker orchestration.

## Canonical gates

- **CI** validates formatting, Clippy, panic-free production targets, workspace tests, documentation, dependency policy, release-evidence fixtures, SBOM generation, and workflow parsing.
- **Daemon** validates daemon lifecycle behavior on Linux, macOS, and Windows.
- **Desktop** validates the React/Tauri frontend, shared runtime adapter, daemon integration, and unsigned cross-platform bundles.
- **Refactor Guardrails** enforces source-size ceilings, workflow permissions, architecture metadata, and this maturity contract.
- **Release Gates** validates coverage, adversarial regressions, fuzzing, chaos recovery, security, packages, documentation/schema consistency, and live-provider scenarios.

A gate name in the manifest must match this retained set. Draft scheduling may defer expensive work, but it does not change capability maturity or promotion requirements.

## Operational boundaries

Platform support is explicit per capability and does not imply identical containment internals. External dependencies are recorded so provider APIs, GitHub services, Node sidecars, or artifact-attestation infrastructure cannot be mistaken for repository-owned guarantees.

README, configuration, compatibility, and release documentation may describe only behavior at or below the recorded maturity. A preview or experimental capability must name its explicit opt-in. A design-only capability must not be presented as available behavior.
