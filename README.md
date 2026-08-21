<p align="center">
  <img src="assets/medusa-banner.png" alt="Medusa — Plan, Execute Safely, Recover" width="100%">
</p>

# Medusa

A local-first, workspace-aware agent written in Rust. Medusa can work in Git repositories, ordinary directories, or explicit ephemeral workspaces. It turns objectives into explicit plans, coordinates bounded specialist agents, isolates mutation, runs guarded commands, verifies results, preserves durable evidence, and resumes work across the CLI, terminal UI, desktop app, daemon, and Telegram.

The product model is **Plan, Execute Safely, Recover**:

- **Plan.** An objective and workspace context become explicit task contracts and a reviewable plan.
- **Execute Safely.** Read-only teammates scout the work. Git mutation can use a conflict-aware bounded implementation DAG with isolated worktrees; ordinary directories use one isolated content-addressed snapshot implementer. Review, independent verification, authorization, and integration remain separate runtime authorities.
- **Recover.** Sessions, plans, events, approvals, worker leases, immutable candidates, delegation contracts, agent scopes, effective model-request manifests, transactions, and verification live under `.medusa` as authoritative state. Interruption, cancellation, or crash never gets rewritten as success.

**Status (v1.0.0, `main`):** CLI, TUI, desktop application, daemon, shared runtime, bounded multi-agent execution, conflict-aware parallel Git mutation, non-Git directory mutation, platform containment, durable sessions, immutable worker delegation contracts, transactional per-agent scopes, durable worker instruction delivery, and effective model-request manifests are shipped. Voice and Telegram implementation foundations are present but their real account/hardware acceptance remains quarantined. Browser model actions are readiness-gated preview with a certified dispatcher; they require explicit opt-in and are not default-enabled. Activation requires `MEDUSA_BROWSER_ENABLED=true`, an explicit `MEDUSA_BROWSER_PATH`, and an admitted `MEDUSA_BROWSER_VERIFY_URL`. The canonical status authorities are `docs/CAPABILITY-CLAIMS.json`, `docs/architecture/baseline.json`, and `docs/provider-support.json`.

Recent `main` work also ships continuous verification with exact-state reuse and drift invalidation, durable coding trajectory and structured repair recovery, evidence-ranked repository context, typed executable skill packages, a certified tool-execution lifecycle, governed continual refinement, read-only live-session observation, scheduled durable actions, and shared semantic execution reporting.

**Out of scope today:** autonomous nested delegation, unconstrained model-driven agent teams, consensus voting, distributed multi-host mutation transactions, non-Git parallel mutation, and any browser, voice, or remote-frontend claim that lacks its required authenticated live evidence.

---

## Contents

- [Why Medusa](#why-medusa)
- [Interfaces](#interfaces)
- [Installation](#installation)
- [Startup and providers](#startup-and-providers)
- [Quick start](#quick-start)
- [Workspace modes](#workspace-modes)
- [Configuration](#configuration)
- [Capabilities and strengths](#capabilities-and-strengths)
- [Architecture](#architecture)
- [Safety and containment](#safety-and-containment)
- [Persistent state and recovery](#persistent-state-and-recovery)
- [Platform support](#platform-support)
- [Current limitations](#current-limitations)
- [Roadmap](#roadmap)
- [Project documentation](#project-documentation)
- [Development](#development)
- [License](#license)

## Why Medusa

Medusa combines an interactive agent product with explicit execution boundaries.

- **Workspace-native.** File, search, command, attachment, memory, and verification capabilities operate around a selected bounded workspace rather than an unrestricted machine-wide shell. Git is an enhanced backend, not a universal prerequisite.
- **Plan, execute safely, recover.** Objectives become task contracts; mutating work is isolated; integration is guarded; failures and interruptions preserve evidence instead of being rewritten as success.
- **Verified completion.** A model response, edit, snapshot, commit, or cherry-pick is not enough. Completion is decided by the configured verification authority for the accepted workspace result.
- **Bounded multi-agent coordination.** Planner and risk-review teammates are read-only. Git implementation may safely decompose into up to three centrally scheduled implementers when the typed mutation DAG proves exact ownership and acceptable conflict risk. Unsafe decomposition falls back to one implementer. Directory mutation uses one isolated snapshot implementer.
- **Immutable delegation authority.** A model-backed worker is bound to a sealed delegation contract before its session is created. Retries reuse that authority and may only lose capabilities when current policy is narrower.
- **Transactional per-agent authority.** Each live agent session has an explicit durable scope covering repository identity, provider profile, execution policy, effective tools, capability registry state, team/member identity, and cancellation ownership. Scope lifecycle is published before model/tool admission and fails closed when stale or stopped.
- **No recursive swarm authority.** Only the root coordinator creates workers. Implementers cannot spawn more implementers, widen their contracts, or integrate their own work.
- **Safe by default.** Writes are path-checked and transactional. Git workspaces use worktree isolation; directory workspaces use immutable content-addressed snapshots, primary-drift detection, and rollback-protected integration. Commands are policy-checked and execute through platform containment that fails closed when unavailable.
- **Durable and inspectable.** Effective model requests are persisted before provider calls with request/provider/scope fingerprints, source-event linkage, delivered session actions, compaction provenance, and tool-schema fingerprints.
- **One runtime, multiple frontends.** CLI, TUI, desktop, daemon clients, and Telegram use the same shared runtime and protocol authorities instead of creating separate agents.
- **Cross-platform Rust core.** The workspace is tested across Linux, macOS, and Windows.

## Interfaces

The interface changes presentation and interaction style; it does not create a separate policy engine, transcript, workspace authority, provider authority, or scheduler.

| Interface | Status | Best for |
|---|---|---|
| **CLI** | Shipped | Automation, CI/CD, scripts, diagnostics, workspace utilities, headless objectives. |
| **Terminal UI (TUI)** | Shipped | Interactive coding, documentation, analysis, plans, approvals, activity, sessions, recovery, metrics, keyboard-first workflows. |
| **Desktop application** | Shipped | Graphical multi-pane workspace with sessions, chat, plans, activity, settings, review, attachments, and voice controls. |
| **Telegram frontend** | Foundation shipped; live acceptance pending | Remote session attachment, mobile status/control, approvals, progressive rendering, files, voice notes, Mini App voice surface. |
| **Daemon** | Shipped | Bounded concurrency, reconnect, cancel-and-drain, IPC control plane for other clients. |
| **Full-duplex voice** | Foundation shipped; live acceptance pending | Provider-neutral realtime core; microphone streaming remains gated to an established supported route. |

### CLI

Run a headless objective:

```bash
medusa run "Fix the failing tests and verify the result"
```

For unattended approval of known shell commands, provide one exact command per line:

```text
# .medusa/approve.txt
cargo test --workspace
cargo fmt --all -- --check
```

```bash
medusa run \
  --non-interactive \
  --approve-allowlist .medusa/approve.txt \
  "Fix the failing tests and verify the result"
```

The allowlist does not bypass policy. Medusa still validates the exact action, active plan, containment, command restrictions, approval scope, and expiry.

Useful commands include:

```bash
medusa doctor
medusa health --json
medusa health --json --support-bundle .medusa/diagnostics/support.json
medusa migrate
medusa update --check
medusa update
medusa search "RuntimeController"
medusa shell cargo test --workspace
medusa checkpoint "before refactor"
medusa resume <session-id>
```

`medusa health` is a bounded, non-billable operational check. It reports typed component status, resource pressure, and durable-journal evidence without treating configuration presence as live provider readiness. `--support-bundle` writes a local, versioned, redacted JSON export; it never uploads data or includes credentials, prompts, hidden reasoning, or authoritative journal payloads.

### Terminal UI

Open the interactive terminal in any bounded working directory:

```bash
cd /path/to/workspace
medusa
```

Useful entry options:

```bash
medusa --repo /path/to/workspace
medusa --prompt "Inspect the material and propose the smallest safe change"
medusa --continue
medusa --resume <session-id>
medusa --fresh
```

`--repo` is retained as the CLI flag for compatibility; the selected path is a **workspace root** and does not need to contain `.git`.

The TUI presents the shared runtime event stream as a conversation and activity timeline. It supports plans, questions, approvals, queued follow-ups, cancellation, session resume, settings, usage metrics, clipboard/file/image attachments, recovery views, team activity, and realtime voice controls. First-run provider setup and `/settings` use the shared provider/model catalog and revision-aware configuration authority, including staged review/apply, diagnostics, deterministic repair where possible, and rollback/history rather than a separate terminal-only configuration store.

### Desktop application

The desktop app is a Tauri/React shell over the same Medusa runtime. It provides session navigation, a central execution timeline, plan and activity presentation, provider/runtime status, settings, attachments, review and learning surfaces, and desktop-native voice controls. Guided first-run onboarding, model discovery/selection, provider defaults, and image-input capability detection consume the same canonical provider/model metadata used by the runtime instead of maintaining independent frontend truth.

### Telegram

Telegram is a frontend to the same authoritative Medusa session, not a separate bot-owned agent. The repository implementation includes Hermes-style rendering, action cards, approvals, durable session attachment/control, files and voice-note handling, and the Mini App voice surface. Real bot/chat/Mini App acceptance remains part of the live evidence tracked by issue [#719](https://github.com/benclawbot/Medusa/issues/719).

See [Telegram](docs/TELEGRAM.md) for setup, service operation, and Mini App wiring.

### Full-duplex voice

Medusa has one provider-neutral realtime voice model rather than a separate voice agent for each frontend. It includes bounded input/output audio queues, partial/final transcripts, voice activity, tool/approval states, reconnect behavior, deterministic resource cleanup, and barge-in that stops spoken output without implicitly cancelling the coding task.

The OpenAI Realtime transport is capability-gated. Live evidence requires the active `openai-oauth` route and an existing ChatGPT login whose trusted account state can establish the bounded Realtime credential flow. Until real account/audio evidence completes issue [#719](https://github.com/benclawbot/Medusa/issues/719), that route remains `external-acceptance-pending` in [`docs/provider-support.json`](docs/provider-support.json).

## Installation

### Prerequisites

- Rust 1.88 or newer for source builds; the repository pins Rust 1.88.0
- A supported model connection for model-dependent work
- The platform containment backend required for guarded shell execution
- Node.js 22 for ChatGPT OAuth, required UI-change browser verification, desktop development, or desktop packaging
- **Git only when needed:** source installation/cloning and Git-backed mutation require Git; packaged Medusa can perform ordinary-directory and ephemeral workspace work without a Git repository

### Install the CLI

The normal install path downloads the current prebuilt release, so it does not compile the Rust workspace. The installer shows only download progress and launches Medusa in the same terminal when installation completes.

Linux/macOS:

```bash
curl -fsSL https://raw.githubusercontent.com/benclawbot/Medusa/main/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/benclawbot/Medusa/main/install.ps1 | iex
```

For development or source-only installation, use Cargo explicitly:

```bash
cargo install --git https://github.com/benclawbot/Medusa.git --locked medusa-cli --quiet
```

### Install from a local checkout

```bash
git clone https://github.com/benclawbot/Medusa.git
cd Medusa
cargo install --path crates/medusa-cli --locked
medusa doctor
```

### Desktop packages

The release workflow produces unsigned packages for Linux (Debian/AppImage), macOS (application archive/DMG), and Windows (NSIS installer). Release assets remain draft-only until a maintainer reviews packages, checksums, SBOM, and provenance. Windows packages are not Authenticode-signed, macOS packages are not Developer ID signed/notarized, and Linux packages are not distributed through a signed package repository.

## Startup and providers

Run Medusa inside a Git repository **or an ordinary directory**:

```bash
cd /path/to/workspace
medusa
```

Interactive startup is provider-agnostic. The TUI and desktop app can open without a configured or currently available model. Existing valid provider profiles are loaded automatically; missing or invalid provider configuration is surfaced in-product so it can be changed without blocking the shell from opening. Provider/model readiness is deferred until a model-dependent action needs it. Headless commands such as `run` or `resume`, which immediately require model execution, may fail fast with a provider-specific readiness error.

The non-secret provider profile is stored in the user configuration directory:

- Linux and macOS: `${XDG_CONFIG_HOME:-~/.config}/medusa/provider.toml`
- Windows: `%APPDATA%\medusa\provider.toml`

API keys are read from the environment and are not written to `provider.toml`.

The canonical selectable-route, support-tier, credential, live-dogfood, and Realtime status matrix is [`docs/provider-support.json`](docs/provider-support.json). Current selectable routes are:

| Route | Support | Credential source |
|---|---|---|
| MiniMax direct | production-supported | `MINIMAX_API_KEY` |
| Anthropic | production-supported | `ANTHROPIC_API_KEY` |
| Anthropic-compatible | custom endpoint | `MEDUSA_API_KEY`, optionally `MEDUSA_BASE_URL` |
| OpenAI API | production-supported | `OPENAI_API_KEY` |
| ChatGPT OAuth | production-supported | `openai-oauth` gateway / existing ChatGPT account state |
| OpenAI-compatible | custom endpoint | `MEDUSA_API_KEY` plus configured endpoint |
| OmniRoute | managed route | managed external route |
| Local runtime | local route | user-operated local runtime |

MiniMax API-key profiles stay on the MiniMax route; they are not reclassified as OpenAI OAuth. OAuth gateway discovery/preflight is used only for explicit OAuth setup/configuration or eager model commands that are actually configured for `openai-oauth`.

ChatGPT OAuth is supplied through the separately distributed `openai-oauth` loopback gateway. See [ChatGPT OAuth](docs/CHATGPT-OAUTH.md):

```bash
npx --yes openai-oauth@latest --detach
```

## Quick start

Open an interactive session:

```bash
medusa
```

Coding in Git:

```bash
cd /path/to/repository
medusa --prompt "Fix the failing tests and verify the result"
```

Documentation in a non-Git directory:

```bash
mkdir -p /tmp/product-docs
medusa --repo /tmp/product-docs --prompt "Create architecture.md from the supplied material and verify the artifact"
```

Analysis over locally available material:

```bash
medusa --repo /path/to/materials --prompt "Compare these sources, cite the evidence, and write report.md"
```

Resume or continue:

```bash
medusa --resume <session-id>
medusa --continue
```

Run headlessly:

```bash
medusa run "Review this workspace and summarize the relevant evidence"
```

Maintenance:

```bash
medusa doctor
medusa migrate
medusa update --check
```

`medusa update --check` is read-only. Source-installed binaries can update from a verified immutable commit on `main`; package-managed installations are not overwritten and instead report the relevant package-manager command.

## Workspace modes

| Mode | Mutation isolation | Parallel mutating implementers | Acceptance identity |
|---|---|---|---|
| **Git** | Dedicated branch/worktree per implementer | Yes, up to three when the conflict-aware DAG accepts the decomposition | Git commit/tree + typed receipts |
| **Directory** | Isolated content-addressed snapshot copy | No; one isolated implementer | `dir-<sha256>` snapshot/tree + typed receipts |
| **Ephemeral** | Medusa-owned temporary directory using the directory backend | No | Content-addressed snapshot/tree until explicit cleanup |

Git parallel mutation is **not** “agents editing the same checkout.” Every child has exact ownership, its own worktree, an immutable delegation contract, an agent scope, independent scope/verification evidence, and no integration authority. Conflicts across manifests, lockfiles, migrations, snapshots, generated outputs, dependencies, or path ownership create ordering/fallback rather than unsafe concurrency. Accepted children pass a durable `IntegrationBarrier` and are composed in deterministic dependency order into a separately verified aggregate candidate before final integration.

Directory mutation snapshots the bounded workspace, rejects symlinks, validates changed components, materializes the immutable candidate separately for independent verification, rejects primary-workspace drift, applies only authorized paths, rolls back on failure, and proves the resulting tree identity. Parallel directory mutation remains intentionally withheld until a separately certified aggregate backend exists.

Programmatic callers can create `medusa_runtime::workspace::Workspace::ephemeral()` for scratch artifacts without starting from a repository. Cleanup is explicit and can only delete a Medusa-owned ephemeral root.

Git is not required for documentation, analysis, supplied-source research, reports, or other bounded artifact work. A non-Git workspace does **not** grant ambient network/browser capabilities: external research still depends on explicitly supported and policy-authorized integrations. Model-executable browser actions are readiness-gated preview with a certified dispatcher; they require explicit opt-in and are not default-enabled.

See [Workspace modes](docs/WORKSPACES.md) and [Multi-agent execution](docs/MULTI_AGENT_EXECUTION.md) for the exact safety rules.

## Configuration

Medusa uses typed TOML configuration with unknown fields denied. Configuration precedence is:

```text
CLI --set overrides
  > environment overrides
  > project TOML
  > user TOML
  > built-in defaults
```

A minimal model configuration looks like:

```toml
version = 1

[agent]
mode = "yolo"
max_turns = 500
parallel_workers = 4

[model]
provider = "minimax"
name = "MiniMax-M3"
protocol = "openai"
auth = "api-key"

[memory]
enabled = true
format = "markdown"

[verification]
required = true
browser_on_ui_change = true
```

`agent.parallel_workers` controls bounded parallel **tool work** in configuration schema v1. It does not authorize autonomous agent recursion and is not the Git mutation-DAG child limit. The conflict-aware mutation implementation has a separate hard safety bound of three mutating children and activates only after typed risk/confidence/scope/resource checks.

Command-line overrides use `--set key=value`:

```bash
medusa --set agent.mode=read-only
medusa --set agent.max_turns=100
medusa --set verification.browser_on_ui_change=false
```

Configuration commands include `medusa config init`, `show`, `edit`, `get`, `set`, `unset`, `validate`, `doctor`, `reset`, and named profile management. Interactive TUI and Desktop configuration consume the same provider catalog/model registry and revisioned profile authority; staged edits can be reviewed before apply and known-good configuration history can be rolled back without exposing credentials. See [Configuration](docs/CONFIGURATION.md) for the canonical schema and migration notes.

## Capabilities and strengths

The 10 capabilities below are recorded as `production` maturity in [`docs/CAPABILITY-CLAIMS.json`](docs/CAPABILITY-CLAIMS.json). Each claim links to owners, production paths, tests, gates, entrypoints, platforms, and promotion criteria.

| # | Capability | Maturity |
|---|---|---|
| 1 | Shared runtime — TUI, desktop, and headless interfaces share one frontend-neutral runtime. | production |
| 2 | Durable sessions & memory — sessions, prompts, memory, provenance, lifecycle, recall. | production |
| 3 | GitHub service — guarded auth and repository workflow operations through a service boundary. | production |
| 4 | Provider context resilience — configuration, role routing, retries, failover, capability authority, typed reasoning exchange, context accounting, and compaction. | production |
| 5 | Identity, approval, transactions — exact-action approvals, dedicated parent review, independent verification, integration authorization, rollback, and durable decisions. | production |
| 6 | Daemon — bounded concurrency, reconnect, cancellation, process-tree termination, graceful drain, recovery. | production |
| 7 | Release trust — validated artifacts, checksums, SBOMs, provenance attestations, draft-only publication. | production |
| 8 | Self-update — verified immutable-main updates that respect package-manager ownership. | production |
| 9 | Multi-agent execution — read-only planner/risk teammates, conflict-aware bounded Git parallel implementers, and isolated directory mutation under one transaction authority. | production |
| 10 | Truthful code-intelligence levels — per-language semantic depth, repository-scoped TypeScript/JavaScript semantics, guarded rename. | production |

### Workspace intelligence

- workspace and file discovery;
- bounded text and symbol search;
- evidence-ranked repository snapshots rebuilt from current repository state for coding turns, including exact symbol/caller/reference ranges, protected policy context, omission reasons, and stable content fingerprints;
- durable coding trajectory across compaction/resume, preserving objective, constraints, relevant/modified paths, decisions, verification requirements, failures, blockers, external evidence, and repository identity while invalidating stale assumptions on drift;
- structured repair ledgers that aggregate and deduplicate diagnostics, retain exact expansion references, link common-root/cascade failures, and track repair attempts and resolution state across verification generations;
- bounded adaptive roadblock recovery that classifies non-progress, suppresses equivalent failed strategies, ranks materially different admissible alternatives, and escalates explicitly when bounded strategy transitions are exhausted;
- context retrieval and turn assembly;
- goals, progress, confidence, continuation, escalation, and failure tracking;
- changed-symbol/component and affected-file analysis where semantic repository evidence exists;
- targeted verification selection with broader fallback checks.

### Agent execution

- durable sessions and objectives;
- explicit plans and task contracts;
- independent read-only planner and risk-review teammates;
- conflict-aware Git mutation DAG with deterministic waves and at most three children;
- one isolated directory/snapshot implementer outside Git;
- immutable delegation contracts persisted before model-backed worker session creation;
- transactional per-agent scopes with explicit prepare/publish/stop lifecycle and resource ownership;
- durable worker/team instructions admitted through session delivery semantics rather than a separate model-context mailbox authority;
- effective model-request manifests persisted before provider calls, including request/provider/scope fingerprints, source-event linkage, delivered session actions, compaction provenance, and tool-schema fingerprints;
- bounded speculative implementation for eligible resolved-scope work, where isolated preparation may overlap upstream review but promotion still fails closed on exact assumptions, repository/scope/policy/dependency evidence, cancellation, and recovery state;
- shared semantic execution reporting that derives progress, completion, verification, blockers, recovery, implementation scope, and final results from canonical journal evidence for consistent frontend projections;
- durable worker leases, epochs, task evidence, child acceptance, aggregate barriers, and cleanup;
- dedicated zero-tool parent review, independent verification, authorization, guarded integration, and reconciliation;
- authoritative primary workspace verification gate.

### Provider routing and context

Provider selection is explicit and role-aware. `model.role_routes` can pin planner, implementer, reviewer, repair, summarization, or formatting phases to configured primary/fallback profiles without silently replacing a user-pinned route. Cross-model context uses the provider-neutral `ReasoningHandoffV1` contract for bounded visible decisions/evidence/verification state; provider-native continuation state remains separately bound to its exact provider/protocol/route/model/session and fails closed when incompatible.

When no user pin overrides route choice, the provider manager can use durable route telemetry for latency, throughput, retry recovery, error categories, downstream verified success, and externally supplied monetary cost. Bounded hedging is eligibility- and budget-gated, publishes only one authoritative winner, and preserves ordinary retry/failover when a hedge is inadmissible or unsuccessful.

### Tools and integrations

- guarded workspace file operations;
- Git-aware change and integration workflows where Git is present;
- policy-controlled command execution;
- a certified typed tool-execution lifecycle with fixed stages, monotonic guard denials, explicit approval state, cancellation normalization, deterministic input fingerprints, and immutable resolved-handler identity across production dispatch paths;
- typed executable skill packages with declared capabilities, repository/network scope, side effects, artifacts, typed JSON I/O, resource budgets, digest-bound validation receipts, contained execution, and fail-closed stale/missing validation;
- a contained per-session analysis workspace for bounded persistent analytical state and fixed-reducer execution without granting repository mutation, credentials, ambient network, or independent provider authority;
- required UI-change browser verification through the internal sidecar; model browser actions are a readiness-gated, explicit-opt-in preview with a certified dispatcher and are not default-enabled;
- image/file prompt attachments when provider capabilities permit;
- MCP and extension boundaries;
- provider routing/fallback chains;
- GitHub, update, and optional Desktop Commander integrations.

### Verification

After mutation, Medusa can inspect typed changed components, select impacted checks, run broader checks when narrow selection would be unsafe, require visible UI evidence for effective interface changes, record evidence/overrides/results, and reject completion when required evidence is absent or failed. A durable verification-completion contract binds required evidence to exact repository state, invalidates scoped evidence after mutation, preserves explicit unavailable/waived/superseded dispositions, and blocks `Completed` until required evidence is fresh.

Authoritative verification executes dependency-aware DAG waves, persists restart-safe checkpoints, reuses prior receipts and warm resources only when complete input/provenance identity still matches, cancels obsolete process trees on repository drift, and retries against refreshed state rather than accepting stale results. The same-model coding harness benchmark corpus protects correctness, verification, resilience, context/recovery quality, and promotion decisions from harness regressions. Directory artifacts without a declared project-level verification command are reported truthfully rather than pretending a command ran; candidate identity, scope, review, independent verification, integration, and resulting tree identity are still enforced.

### Memory and learning

Medusa supports workspace-scoped Markdown memory, bounded recall with provenance, memory consolidation/writeback, verified-session learning/probationary lessons, failure history/negative outcomes, and a rule that optimistic or unverified completion cannot become accepted positive experience.

Continual refinement is evidence-gated: typed proposals preserve provenance, deterministic evaluation and explicit approval precede activation, security/authority roots stay outside refinable content, activation history is append-only, and exact rollback is retained. User corrections and accepted runtime signals feed a privacy-filtered typed provenance graph and evaluated correction loop rather than self-activating directly. Effectiveness monitoring attributes outcomes only to actually selected refinements, keeps confounded evidence explicit, decays confidence on drift, and can route harmful refinements to rollback; capability, policy, harness, evaluator, and other protected-boundary changes still go through ordinary engineering review.

Production behavioral learning follows one result-authoritative lifecycle: **execute -> independently verify -> record outcome -> compare comparable cohorts -> detect improvement/regression -> test bounded adaptation -> independently re-verify -> promote or roll back**. Model or worker claims such as “fixed,” “tests pass,” or confidence are observations, not correctness authority; independently verified root-task outcomes are the ground truth. Failed, cancelled, partial, censored, and inconclusive runs remain evidence, and cost is reported only when an authoritative pricing observation exists. Bounded low-risk policies may be canaried or promoted only when objective evidence proves improvement in verified success, repair burden, latency, or cost per independently verified successful task; harmful or regressing changes retain an exact predecessor for suspension or rollback. The automatic loop cannot expand capability, containment, credentials, approvals, mutation, evaluator, or verification authority, and protected or high-impact changes remain in ordinary engineering review. The repository currently ships the typed routing, tool-learning, controller, and shared-health contracts; end-to-end production acceptance remains separately gated until live evidence exists.

### Observability and resilience

Typed runtime events cover usage, progress, activity, team, plan, question, completion, cancellation, failure, and recovery state. Shared execution reporting collapses low-level activity into deterministic semantic updates across Headless, TUI, Desktop, Telegram, and future frontends. Read-only live-session observation reconstructs bounded stage/plan/tool/file/verification/blocker state from the durable journal, and side questions over that snapshot cannot steer, mutate, approve, or cancel the primary run.

Scheduled timer/heartbeat/file/process/external-signal wakeups enter the same durable session-action authority with idempotent occurrence provenance and explicit busy-session semantics instead of creating a scheduler-owned prompt queue. Process registry/supervision, generation-bound process identity, checkpoints, replay, time travel, transactions, continuity, deterministic cancellation/resource cleanup, operational health/support bundles, repository-wide lifecycle/privacy certification, and resilience fault campaigns provide the recovery foundation.

## Architecture

<p align="center">
  <img src="docs/assets/medusa-architecture.jpg" alt="Medusa architecture: interfaces, shared runtime, multi-agent execution, tools and policy, state and recovery, memory and learning, containment, and a shared authoritative data layer" width="100%">
</p>

The canonical production path is:

```text
CLI / TUI / Desktop / daemon frontend / Telegram
  -> typed frontend command
  -> RuntimeController
  -> production task contracts
  -> MultiAgentCoordinator
  -> read-only planner and risk reviewer
  -> mutation required?
       -> Git + safe decomposition: bounded mutation DAG -> isolated child worktrees -> IntegrationBarrier -> aggregate candidate
       -> Git fallback: one isolated worktree implementer
       -> directory/ephemeral: one isolated content-addressed snapshot implementer
  -> dedicated zero-tool parent review
  -> independent immutable-candidate verification
  -> integration authorization
  -> guarded integration + reconciliation
  -> primary workspace verification gate
  -> typed events and durable evidence
```

For each model-backed session or worker, admission/request authority is enforced at the point that session is created and each request is sent:

```text
delegated worker only: sealed DelegationContract before session creation
  -> prepare and publish AgentScope before model/tool admission
  -> admit durable session instruction/action state
  -> persist effective model-request manifest before provider call
  -> execute the provider attempt under the bound scope/request authority
```

Those per-session invariants strengthen the production path; they do not create recursive delegation or a second orchestration authority.

### Major layers

| Layer | Responsibilities | Principal crates |
|---|---|---|
| **Interfaces** | CLI parsing, terminal interaction, desktop UI, Telegram command/rendering | `medusa-cli`, `medusa-tui`, `apps/medusa-desktop`, daemon Telegram modules |
| **Runtime authority** | Session lifecycle, commands, events, coordination, completion, cancellation, agent scopes | `medusa-runtime`, `medusa-agent`, `medusa-daemon` |
| **Multi-agent execution** | Task contracts, immutable delegation, scheduling, leases, mutation DAGs, isolated implementation, barriers, parent review | `medusa-multi-agent-scheduler`, `medusa-workers`, `medusa-worker-leases`, runtime coordinators |
| **Context and intelligence** | Workspace context, retrieval, turn assembly, goals, progress, confidence, failure | context and intelligence crate families |
| **Tools and policy** | Capability discovery, authorization, execution control, Git/browser/extensions | capability, policy, control, extension, GitHub, and browser crates |
| **State and recovery** | Sessions, request manifests, checkpoints, replay, time travel, continuity, transactions, recovery | agent/session, checkpoint, replay, time-travel, continuity, transaction, recovery crates |
| **Memory and improvement** | Markdown memory, consolidation, writeback, learning, hardening | memory, improvement, and hardening crate families |
| **Containment** | Platform sandboxing, process ownership, limits, cleanup | `medusa-process-containment`, `medusa-process-registry`, `medusa-runtime-supervisor` |
| **Protocol and providers** | Typed frontend/event contracts, model routes, role routing, reasoning exchange, Realtime voice contracts | `medusa-protocol`, `medusa-provider`, `medusa-openai-realtime` |

For source-level ownership, see [Product architecture](docs/ARCHITECTURE.md), [Production execution trace](docs/PRODUCTION-EXECUTION-TRACE.md), [Contributor architecture](docs/CONTRIBUTOR-ARCHITECTURE.md), and [Workspace modes](docs/WORKSPACES.md).

## Safety and containment

Medusa is intentionally not an unrestricted shell replacement.

### Workspace writes

Writes resolve against the selected workspace and remain policy checked, transactional, and evidence-bearing. Git mutation preserves symlink semantics through Git/worktree isolation. Directory mutation fails closed on symlinks rather than copying an ambiguous filesystem graph. Sensitive locations remain denied, including `.git` internals, credential stores, operating-system configuration/executable paths, and login-persistence locations.

### Command containment

Shell execution fails closed if the platform backend is unavailable:

- **Linux:** Bubblewrap
- **macOS:** Seatbelt
- **Windows:** Windows 11 composable sandbox API with workspace binding, toolchain read-only binding, network denial, environment allowlisting, and Job Object limits

Windows command containment requires Windows 11 with `Experimental_CreateProcessInSandbox`. There is no unsandboxed fallback through that API.

### Agent and worker authority

Agent scope and delegation are explicit runtime authority, not prompt convention. A scope binds the live session to repository identity, provider profile, execution policy, capability registry fingerprint, effective tools, team/member identity, and cancellation ownership. Worker delegation additionally seals task/lease/repository/worktree/read-write/tool/model/budget/evidence authority. A retry can be narrower than its sealed contract, but cannot widen it.

### Approvals

Approvals bind to structured actions and current runtime state. Exact command allowlists, interactive approve-once decisions, Telegram callback foundations, expiry, idempotency, and plan fingerprints do not weaken policy or containment.

### Cancellation and cleanup

Cancellation propagates through runtime, model, tool, process, worker, transaction, and frontend state. Process ownership and containment terminate child process trees and preserve durable cancellation/failure evidence. Dropping live agent-scope ownership closes new cancellable admission; durable terminal publication remains explicit runtime state.

## Persistent state and recovery

Workspace-local state lives under `.medusa`. Durable authority categories include:

- sessions, objectives, transcript/events, plans, task contracts, questions, and approvals;
- provider/tool/integration/verification evidence;
- effective model-request content/manifests and provider-attempt lineage;
- worker/team session actions and model-visibility linkage;
- immutable delegation contracts and retry/attempt bindings;
- transactional agent-scope contracts, generations, lifecycle, revocations, and owned-resource state;
- coding trajectory checkpoints, structured repair ledgers, Compaction Manifest V2 records, and advisory fingerprint-bound abandoned-branch summaries;
- verification DAG checkpoints, exact-state reusable verification receipts, warm-resource descriptors, and repository-drift invalidation evidence;
- continual-refinement proposals/activation history, correction-loop episodes, privacy-filtered provenance/effectiveness evidence, and rollback state;
- scheduled trigger occurrence/dispatch provenance admitted into durable session actions;
- worker leases, epochs, isolated candidates, Git commit or directory snapshot receipts;
- checkpoints, replay, time travel, transaction/review/authorization/rollback records;
- failure/recovery decisions, memory/learning, and frontend continuity.

Resume and recovery never treat display text or an optimistic model response as authoritative execution evidence. Model-visible worker instructions are tied to durable session/action state and effective request evidence rather than a standalone mailbox boolean. Schema names such as `prepared_commit` and `prepared_tree` are retained for compatibility; in directory workspaces they hold content-addressed snapshot/tree identifiers rather than Git object IDs.

## Platform support

Canonical workflows test the Rust workspace and daemon behavior across Linux, macOS, and Windows. Desktop CI builds and validates unsigned packages on all three platforms. Parallel Mutation Certification proves the Git multi-implementer path cross-platform; workspace-backend tests cover non-Git isolation and drift-safe integration.

Repository gates cover formatting, Clippy, tests, documentation, dependency/security policy, architecture drift, containment regressions, adversarial cases, fuzz smoke tests, migration/chaos recovery, package smoke tests, and selected live-provider scenarios.

Platform support does not imply identical containment, audio, browser, credential-store, or operating-system signing behavior.

## Current limitations

- Autonomous nested delegation, unconstrained dynamic agent teams, consensus voting, and distributed multi-host mutation transactions are not supported.
- Conflict-aware parallel **mutation** currently requires a Git workspace; directory/ephemeral workspaces deliberately use one isolated snapshot implementer.
- Directory mutation fails closed on symlink-bearing workspaces; use Git mutation when symlink semantics must be preserved.
- OpenAI Realtime and Telegram end-to-end acceptance still require real ChatGPT OAuth, audio hardware, bot/chat/Mini App access, and sanitized evidence under issue [#719](https://github.com/benclawbot/Medusa/issues/719).
- ChatGPT OAuth depends on the separately distributed `openai-oauth` gateway and Node.js.
- Browser model actions are readiness-gated, explicit-opt-in preview; the dispatcher is certified-production, but the capability is not default-enabled and remains bounded by route admission, permissions, and required verification authority.
- Anthropic-family provider requests in the current adapter are non-streaming (`capabilities.streaming = false` and `"stream": false`).
- Screenshot input is accepted only when the selected provider declares compatible image support and limits.
- Desktop release packages are unsigned at the operating-system level.
- Windows command containment requires the Windows 11 composable sandbox API.

## Roadmap

Repository implementation work that the previous README listed as future roadmap items has landed on `main`, including long-context/compaction work, analysis workspace and branch-summary support, skills/refinement/observer/scheduled-action work, resilience/privacy/operational hardening, self-improvement integration, continuous verification/resource warming, bounded speculative implementation, coding trajectory/repair recovery, certified tool execution, durable worker instruction/request authority, transactional agent scopes, immutable delegation retries, and durable-journal/runtime hot-path work.

The remaining manual/live acceptance tracked for shipped-but-quarantined remote/voice functionality is:

- [#719](https://github.com/benclawbot/Medusa/issues/719) — OpenAI Realtime voice and Telegram end-to-end proof using real accounts, bot/chat access, microphone/audio hardware, and sanitized evidence.

GitHub issues are the source of truth for newly opened work; the README does not treat closed implementation issues as future roadmap items.

## Project documentation

- [Product architecture](docs/ARCHITECTURE.md)
- [Workspace modes](docs/WORKSPACES.md)
- [Multi-agent execution](docs/MULTI_AGENT_EXECUTION.md)
- [Mutating worktree integration](docs/MUTATING-WORKTREE-INTEGRATION.md)
- [Contributor architecture](docs/CONTRIBUTOR-ARCHITECTURE.md)
- [Production execution trace](docs/PRODUCTION-EXECUTION-TRACE.md)
- [Configuration](docs/CONFIGURATION.md)
- [Provider support](docs/PROVIDER-SUPPORT.md)
- [Capability claims](docs/CAPABILITY-CLAIMS.json)
- [Capability evidence](docs/CAPABILITY-EVIDENCE.md)
- [Session action plane](docs/architecture/session-action-plane.md)
- [Durable journal policy](docs/durable-journal-policy.md)
- [Repository indexing](docs/repository-indexing.md)
- [Tool execution pipeline](docs/TOOL-EXECUTION-PIPELINE.md)
- [Refinement authority migration](docs/refinement-authority-migration.md)
- [Resilience certification](docs/resilience-certification.md)
- [Data lifecycle certification](docs/data-lifecycle-certification.md)
- [Benchmarks](docs/BENCHMARKS.md)
- [Observability](docs/OBSERVABILITY.md)
- [Security hardening](docs/SECURITY-HARDENING.md)
- [Desktop distribution](docs/DESKTOP-DISTRIBUTION.md)
- [Release process](docs/RELEASE.md)
- [Release compatibility](docs/COMPATIBILITY.md)
- [Telegram](docs/TELEGRAM.md)
- [Live self-improvement acceptance](docs/LIVE-SELF-IMPROVEMENT-ACCEPTANCE.md)

## Development

Use the pinned toolchain and run the gates relevant to the change:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo doc --workspace --all-features --no-deps
```

Architecture or capability changes must also pass:

```bash
python3 scripts/check-product-architecture.py
python3 scripts/check-capability-evidence.py
```

Parallel mutation changes must preserve `.github/workflows/parallel-mutation-certification.yml`. Workspace-backend changes must preserve Git behavior and cross-platform non-Git isolation/drift tests. Frontend/desktop changes must pass the checks under `apps/medusa-desktop`; Telegram changes must preserve shared runtime authority, numeric authorization, callback replay safety, idempotency, redaction, and deterministic renderer tests.

## License

MIT.
