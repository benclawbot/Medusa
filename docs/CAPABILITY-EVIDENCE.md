# Medusa Capability Evidence

This document is the durable evidence ledger for capabilities represented on `main`. The machine-readable legacy-availability authority is [`CAPABILITY-CLAIMS.json`](CAPABILITY-CLAIMS.json), validated by `scripts/check-capability-evidence.py`.

Architecture-v2 certification is separate and is governed by [`architecture/INDEX.md`](architecture/INDEX.md) and [`architecture/baseline.json`](architecture/baseline.json). A legacy `production` value means a supported current entrypoint exists; it does **not** certify a capability beyond the v2 authority, lifecycle, dispatcher, review, verification, provider, trust-boundary, or deletion requirements.

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
| `github-service` | `production` | integration maintainers | guarded GitHub workflows | Linux, macOS, Windows | GitHub API |
| `provider-context-resilience` | `production` | provider maintainers | `medusa`, `medusa run`, `medusa quickstart` | Linux, macOS, Windows | configured model provider |
| `identity-approval-transactions` | `production` | safety maintainers | `medusa`, `medusa run` | Linux, macOS, Windows | none |
| `daemon` | `production` | daemon maintainers | daemon and desktop adapter | Linux, macOS, Windows | none |
| `release-trust` | `production` | release maintainers | publish-release workflow | Linux, macOS, Windows | GitHub artifact attestations |
| `self-update` | `production` | CLI maintainers | `medusa update` | Linux, macOS, Windows | GitHub repository access |
| `multi-agent-research` | `production` | agent runtime maintainers | coordinated `run_prompt` preflight, conflict-aware Git parallel mutation, and isolated directory implementation | Linux, macOS, Windows | configured model provider; Git only for Git-backed parallel mutation |
| `truthful-code-intelligence-levels` | `production` | code intelligence maintainers | `semantic_capabilities`, `code_index`, `typescript_semantic`, `symbol_rename` | Linux, macOS, Windows | `typescript-language-server` for TypeScript/JavaScript semantic operations |

The manifest records current production paths, behavioral test paths, canonical gates, observability references, public documentation, promotion evidence, default activation, explicit opt-ins, and capability dependencies. The v2 baseline additionally records dispositions, exact blockers, source-of-truth ownership, trust boundaries, migration consumers, and legacy deletion targets.

### Multi-agent and workspace evidence

The `multi-agent-research` production claim now covers two mutation backends under one transaction authority:

- **Git:** `parallel_mutation` builds a typed conflict-aware `MutationDag` only for exact, sufficiently confident, non-high-risk scopes within the bounded three-mutator budget. Specialized resources cover manifests, lockfiles, migrations, snapshots, and generated outputs. `parallel_mutation_batch` independently accepts child evidence, establishes `IntegrationBarrier`, deterministically stages accepted children, validates aggregate scope, verifies the aggregate, and prepares the final immutable transaction. `.github/workflows/parallel-mutation-certification.yml` exercises DAG behavior, runtime wiring, deterministic integration, rollback/cleanup, fallback/scope invalidation, and performance evidence across Linux, macOS, and Windows.
- **Directory / ephemeral:** `workspace_worker_manager` fingerprints the bounded directory, creates one isolated copy, derives typed changed components, persists content-addressed baseline/candidate snapshots, materializes the immutable candidate for independent verification, rejects primary drift, applies only authorized paths, rolls back failed application, and proves resulting tree identity. Directory mutation fails closed on symlinks. `workspace.rs` exposes Git/directory detection and an explicitly owned ephemeral-workspace lifecycle.

Read-only planner/risk-review teammate coordination is independent of Git. Git is therefore an external dependency only for Git-backed worktree and parallel-mutation semantics, not for general documentation, analysis, supplied-source research, or non-Git artifact mutation.

## Architecture v2 certification authority

Architecture-v2 migration and certification are governed only by [`architecture/INDEX.md`](architecture/INDEX.md) and [`architecture/baseline.json`](architecture/baseline.json). This legacy availability ledger no longer reproduces a second certification table or migration-status narrative; doing so previously left completed work described as pending.

The architecture authority records the certified shared runtime, durable state, guarded mutation lifecycle, provider health, release trust, containment, frontend projection, and other production boundaries. It also records the current exceptions: model-executable browser actions remain quarantined, plugins/extensions remain preview unless individually certified, and Telegram duplex/audio behavior remains quarantined pending real external evidence.

Provider route, dogfood, credential, and Realtime status are separately governed by [`provider-support.json`](provider-support.json). A legacy `production` availability entry here cannot promote a route or capability beyond either machine-readable authority.

## Planned and scaffolding behavior

### Remaining design-only boundary

The current coordinated path supports bounded Git multi-implementer mutation and one isolated mutating implementer for directory/ephemeral workspaces. It does **not** support autonomous nested delegation, unconstrained model-driven team creation, consensus voting, distributed multi-host transaction coordination, or non-Git parallel mutation. Those capabilities require a production caller, one durable state authority, recovery path, observability contract, permissions, and behavioral proof before promotion.

The contained analysis-workspace/recursive-delegation roadmap must not be read as evidence that recursive delegation is already active. Current implementation children cannot spawn implementation children or widen their own contracts.

Browser and plugin structure must not be presented as active capability merely because crates, schemas, or tool definitions exist. Architecture v2 requires definition → readiness → permission → dispatch → side effect → evidence → event delivery → cleanup conformance.

## Canonical gates

- **CI** validates formatting, Clippy, panic-free production targets, workspace tests, documentation, dependency policy, release-evidence fixtures, SBOM generation, and workflow parsing.
- **Daemon** validates daemon lifecycle behavior on Linux, macOS, and Windows.
- **Desktop** validates the React/Tauri frontend, shared runtime adapter, daemon integration, and unsigned cross-platform bundles.
- **Refactor Guardrails** enforces workflow permissions, current architecture metadata, and legacy maturity contracts.
- **Architecture v2 Baseline** validates the living index, workspace/component inventory, production paths, duplicate authorities, forbidden dependencies, PR governance, CODEOWNERS, real CLI entrypoints, and removable expected-failure fixtures on Linux, macOS, and Windows.
- **Release Gates** validates coverage, adversarial regressions, fuzzing, chaos recovery, security, packages, documentation/schema consistency, and live-provider scenarios.
- **Parallel Mutation Certification** proves the bounded conflict-aware Git mutator DAG, deterministic aggregate barrier, rollback/cleanup, fallback, scope invalidation, and performance behavior cross-platform.
- **Workspace Backend Certification** proves non-Git content-addressed isolation, public workspace lifecycle, architecture claims, and capability evidence cross-platform.
- **Code Intelligence Certification** installs the production TypeScript language server and validates formatting, linting, correctness/freshness fixtures, production agent tests, benchmark compilation/execution, and architecture ownership on Linux, macOS, and Windows for the final issue-closing PR.

## Operational boundaries

Platform support is explicit per capability and does not imply identical containment internals. External dependencies are recorded so provider APIs, Git services, Node sidecars, or artifact-attestation infrastructure cannot be mistaken for workspace-owned guarantees. README, configuration, compatibility, release documentation, and UI labels may describe only behavior at or below the recorded legacy availability and v2 certification.

- The `truthful-code-intelligence-levels` claim is recorded in the maturity matrix above and in `CAPABILITY-CLAIMS.json`. Its typed profiles, registry permissions, production dispatch, deterministic freshness evidence, guarded mutation path, architecture record, and benchmark must remain synchronized.
