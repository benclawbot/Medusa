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

- **Git:** `parallel_mutation` builds a typed conflict-aware `MutationDag` only for exact, sufficiently confident, non-high-risk scopes within the bounded three-mutator budget. Specialized resources cover manifests, lockfiles, migrations, snapshots, and generated outputs. `parallel_mutation_batch` independently accepts child evidence, establishes `IntegrationBarrier`, deterministically stages accepted children, validates aggregate scope, verifies the aggregate, and prepares the final immutable transaction. The `Deterministic benchmarks and certification corpus` job of `.github/workflows/ci.yml` exercises DAG behavior, runtime wiring, deterministic integration, rollback/cleanup, fallback/scope invalidation, and performance evidence across Linux, macOS, and Windows.
- **Directory / ephemeral:** `workspace_worker_manager` fingerprints the bounded directory, creates one isolated copy, derives typed changed components, persists content-addressed baseline/candidate snapshots, materializes the immutable candidate for independent verification, rejects primary drift, applies only authorized paths, rolls back failed application, and proves resulting tree identity. Directory mutation fails closed on symlinks. `workspace.rs` exposes Git/directory detection and an explicitly owned ephemeral-workspace lifecycle.

Read-only planner/risk-review teammate coordination is independent of Git. Git is therefore an external dependency only for Git-backed worktree and parallel-mutation semantics, not for general documentation, analysis, supplied-source research, or non-Git artifact mutation.

## Architecture v2 certification authority

Architecture-v2 migration and certification are governed only by [`architecture/INDEX.md`](architecture/INDEX.md) and [`architecture/baseline.json`](architecture/baseline.json). This legacy availability ledger no longer reproduces a second certification table or migration-status narrative; doing so previously left completed work described as pending.

The architecture authority records the certified shared runtime, durable state, guarded mutation lifecycle, provider health, release trust, containment, frontend projection, and other production boundaries. Managed plugins/extensions remain preview unless individually certified.

Provider route, dogfood, and credential status are separately governed by [`provider-support.json`](provider-support.json). A legacy `production` availability entry here cannot promote a route or capability beyond either machine-readable authority.

## Planned and scaffolding behavior

### Remaining design-only boundary

The current coordinated path supports bounded Git multi-implementer mutation and one isolated mutating implementer for directory/ephemeral workspaces. It does **not** support autonomous nested delegation, unconstrained model-driven team creation, consensus voting, distributed multi-host transaction coordination, or non-Git parallel mutation. Those capabilities require a production caller, one durable state authority, recovery path, observability contract, permissions, and behavioral proof before promotion.

The contained analysis-workspace/recursive-delegation roadmap must not be read as evidence that recursive delegation is already active. Current implementation children cannot spawn implementation children or widen their own contracts.

Plugin structure must not be presented as active capability merely because crates, schemas, or tool definitions exist. Architecture v2 requires definition → readiness → permission → dispatch → side effect → evidence → event delivery → cleanup conformance.

## Canonical gates

All validation runs in one workflow, `.github/workflows/ci.yml`. The gates below are its jobs.

- **Workspace quality** validates formatting, Clippy, panic-free production targets, workspace tests, dependency authority, and the unsafe-rust boundary.
- **Dependency policy** validates unused dependencies, the committed lockfile, release evidence fixtures, SBOM generation, dependency-metric drift, and the parsed tag-only release workflow.
- **Repository policy and evidence** enforces workflow permissions, immutable release-workflow policy, the release keyring, architecture policy, the engineering-policy engine, the certified tool pipeline, architecture ownership and trust boundaries, real CLI entrypoint conformance, capability evidence, workspace-surface claims, and provider support/delivery contracts.
- **Documentation and public API** builds docs with warnings denied, reconciles the documentation inventory, validates release evidence and required documents, and compares governed public APIs against the base revision.
- **Platform suites** validates daemon and TUI lifecycle, directory mutation and workspace lifecycle backends, the recording-provider multimodal contract, data lifecycle, and deterministic resilience primitives on Linux, macOS, and Windows.
- **Acceptance, adversarial regressions, and security** runs shared-target PR smoke acceptance, containment escape regressions, the reproducible safety and recovery proof, fuzz and chaos campaigns, provider delivery diagnostics, and external repository contracts.
- **Workspace coverage >= 75%** enforces the workspace line-coverage floor.
- **Deterministic benchmarks and certification corpus** validates benchmark scoring fixtures, runs the release-blocking reliability, orchestration, and same-model coding-harness benchmarks, and certifies the performance corpus, continuous verification, parallel mutation, compound-tool DAG, and speculative execution contracts.
- **Desktop adapter** lints and tests the Tauri adapter on Linux, macOS, and Windows.
- **Desktop frontend and bundles** validates synchronized desktop versions, typechecks, tests, and builds the frontend, prepares verified Tauri bundler tools, and emits SHA-256 bundle evidence.
- **Package and quickstart smoke** builds the release package, smoke-tests it, and runs the deterministic quickstart on Linux, macOS, and Windows.
- **Live provider and product gates** runs the credential-gated live provider dogfood and interactive TUI gates. It is reachable only through `workflow_dispatch`, so no pull request can reach repository credentials.

Release publication and signing remain separate by design: `publish-release.yml`, `sign-release-manifest.yml`, `sign-release-manifest-recovery.yml`, `sign-draft-release.yml`, `rolling-main-cli.yml`, `verify-published-release.yml`, `release-recovery.yml`, `verified-prebuilt-update.yml`, and `unsafe-rust-boundary.yml` run on tags, releases, or manual dispatch only.

## Operational boundaries

Platform support is explicit per capability and does not imply identical containment internals. External dependencies are recorded so provider APIs, Git services, Node sidecars, or artifact-attestation infrastructure cannot be mistaken for workspace-owned guarantees. README, configuration, compatibility, release documentation, and UI labels may describe only behavior at or below the recorded legacy availability and v2 certification.

- The `truthful-code-intelligence-levels` claim is recorded in the maturity matrix above and in `CAPABILITY-CLAIMS.json`. Its typed profiles, registry permissions, production dispatch, deterministic freshness evidence, guarded mutation path, architecture record, and benchmark must remain synchronized.
