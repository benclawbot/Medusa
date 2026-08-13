<p align="center">
  <img src="assets/medusa-banner.png" alt="Medusa — Plan, Execute Safely, Recover" width="100%">
</p>

# Medusa

A local-first, workspace-aware agent written in Rust. Medusa can work in Git repositories, ordinary directories, or explicit ephemeral workspaces. It turns objectives into explicit plans, coordinates bounded specialist agents, isolates mutation, runs guarded commands, verifies results, preserves durable evidence, and resumes work across the CLI, terminal UI, desktop app, daemon, and Telegram.

The product model is **Plan, Execute Safely, Recover**:

- **Plan.** An objective and workspace context become explicit task contracts and a reviewable plan.
- **Execute Safely.** Read-only teammates scout the work. Git mutation can use a conflict-aware bounded implementation DAG with isolated worktrees; ordinary directories use one isolated content-addressed snapshot implementer. Review, independent verification, authorization, and integration remain separate runtime authorities.
- **Recover.** Sessions, plans, events, approvals, worker leases, immutable candidates, transactions, and verification live under `.medusa` as authoritative state. Interruption, cancellation, or crash never gets rewritten as success.

**Status (v1.0.0, `main`):** CLI, TUI, desktop application, daemon, shared runtime, bounded multi-agent execution, conflict-aware parallel Git mutation, non-Git directory mutation, platform containment, and durable sessions are shipped. Voice and Telegram implementation foundations are present but their real account/hardware acceptance remains quarantined; model-executable browser actions remain withheld. The canonical status authorities are `docs/CAPABILITY-CLAIMS.json`, `docs/architecture/baseline.json`, and `docs/provider-support.json`.

**Out of scope today:** autonomous nested delegation, unconstrained model-driven agent teams, consensus voting, distributed multi-host mutation transactions, non-Git parallel mutation, and any browser, voice, or remote-frontend claim that lacks its required authenticated live evidence.

---

## Contents

- [Why Medusa](#why-medusa)
- [Interfaces](#interfaces)
- [Installation](#installation)
- [First run](#first-run)
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
- **Bounded multi-agent coordination.** Planner and risk-review teammates are read-only. Git implementation may safely decompose into up to three centrally scheduled implementers when the typed mutation DAG proves exact ownership and acceptable conflict risk. Unsafe decomposition falls back to one implementer. Directory mutation always uses one isolated snapshot implementer in this release.
- **No recursive swarm authority.** Only the runtime coordinator creates mutating workers. Implementers cannot spawn more implementers, widen their contracts, or integrate their own work.
- **Safe by default.** Writes are path-checked and transactional. Git workspaces use worktree isolation; directory workspaces use immutable content-addressed snapshots, primary-drift detection, and rollback-protected integration. Commands are policy-checked and execute through platform containment that fails closed when unavailable.
- **Durable and inspectable.** Sessions, plans, events, approvals, verification evidence, worker receipts, transactions, memory, checkpoints, candidates, and recovery state live under `.medusa`.
- **One runtime, multiple frontends.** CLI, TUI, desktop, daemon clients, and Telegram use the same shared runtime and protocol authorities instead of creating separate agents.
- **Cross-platform Rust core.** The workspace is tested across Linux, macOS, and Windows.

## Interfaces

The interface changes presentation and interaction style; it does not create a separate policy engine, transcript, workspace authority, or scheduler.

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

Other commands:

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

`--repo` is retained as the CLI flag for compatibility; the selected path is now a **workspace root** and does not need to contain `.git`.

The TUI presents the shared runtime event stream as a conversation and activity timeline. It supports plans, questions, approvals, queued follow-ups, cancellation, session resume, settings, usage metrics, clipboard/file/image attachments, recovery views, team activity, and realtime voice controls.

### Desktop application

The desktop app is a Tauri/React shell over the same Medusa runtime. It provides session navigation, a central execution timeline, plan and activity presentation, provider/runtime status, settings, attachments, review and learning surfaces, and desktop-native voice controls.

### Telegram

Telegram is a frontend to the same authoritative Medusa session, not a separate bot-owned agent. The implementation from issue [#568](https://github.com/benclawbot/Medusa/issues/568) ships the Hermes-style rendering, action card, approval, and full-duplex voice Mini App surface. Real bot/chat/Mini App acceptance remains part of the quarantined live evidence tracked by the current provider-support authority.

Shipped foundations include versioned frontend contracts, numeric-identity default-deny authorization, replay-safe rendering, one-shot approval callbacks, durable bindings/cursors/preferences, shared daemon/runtime routing, Telegram voice notes/TTS bubbles, and the authenticated Mini App voice surface pending real network/audio acceptance evidence.

See [Telegram](docs/TELEGRAM.md) for setup, service operation, and Mini App wiring.

### Full-duplex voice

Medusa has one provider-neutral realtime voice model rather than a separate voice agent for each frontend. It includes bounded input/output audio queues, partial/final transcripts, voice activity, tool/approval states, reconnect behavior, deterministic resource cleanup, and barge-in that stops spoken output without implicitly cancelling the coding task.

| Surface | Capability |
|---|---|
| **TUI** | Full-duplex controller with `/voice`, `/voice off`, `/mute`, `/unmute`, `/stop-speech`, `/cancel-response`, and `/cancel-task`. |
| **Desktop** | Compact voice entry, explicit microphone permission, mute/speaker controls, device selection, reconnect, transcripts, barge-in, deterministic cleanup. |
| **Telegram** | Durable voice-mode preferences, voice notes, TTS voice bubbles, and authenticated Mini App access to the shared voice session. |

The provider transport is capability-gated. Live OpenAI Realtime evidence requires the active `chatgpt-oauth` / `openai-oauth` profile and an existing ChatGPT login whose trusted Codex account state can mint a bounded short-lived Realtime credential. Medusa establishes that credential before microphone permission and does not request or persist a separate voice API key. Until real account/audio evidence completes issue #719, the route remains `external-acceptance-pending` in `docs/provider-support.json`.

## Installation

### Prerequisites

- Rust 1.88 or newer for source builds; the repository pins Rust 1.88.0
- A supported model connection
- The platform containment backend required for guarded shell execution
- Node.js 22 for ChatGPT OAuth, required UI-change browser verification, desktop development, or desktop packaging
- **Git only when needed:** source installation/cloning and Git-backed mutation require Git; packaged Medusa can perform ordinary-directory and ephemeral workspace work without a Git repository

### Install the CLI from `main`

This source-install command itself uses Git:

```bash
cargo install --git https://github.com/benclawbot/Medusa.git --locked medusa-cli
```

Confirm installation:

```bash
medusa --version
medusa doctor
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

### Telegram

The Telegram frontend ships under the daemon. Setup, service operation, Mini App wiring, and live acceptance are documented in [Telegram](docs/TELEGRAM.md).

## First run

Run Medusa inside a Git repository **or an ordinary directory**:

```bash
cd /path/to/workspace
medusa
```

The first interactive launch asks for a model connection and stores the non-secret profile in the user configuration directory:

- Linux and macOS: `${XDG_CONFIG_HOME:-~/.config}/medusa/provider.toml`
- Windows: `%APPDATA%\medusa\provider.toml`

API keys are read from the environment and are not written to `provider.toml`.

The canonical selectable-route, support-tier, credential, live-dogfood, and Realtime status matrix is [`docs/provider-support.json`](docs/provider-support.json); the rendered [provider support guide](docs/PROVIDER-SUPPORT.md) is checked against it in CI.

| Route | Credential |
|---|---|
| MiniMax | `MINIMAX_API_KEY` |
| Anthropic | `ANTHROPIC_API_KEY` |
| Anthropic-compatible endpoint | `MEDUSA_API_KEY`, optionally `MEDUSA_BASE_URL` |

The setup/provider layer also supports configured OpenAI-compatible gateways, local model runtimes, OmniRoute, the OpenAI API, and ChatGPT OAuth where their advertised capabilities meet the selected workflow.

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

Analysis/research over locally available material:

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

Git parallel mutation is **not** “agents editing the same checkout.” Every child has exact ownership, its own worktree, independent scope/verification evidence, and no integration authority. Conflicts across manifests, lockfiles, migrations, snapshots, generated outputs, dependencies, or path ownership create ordering/fallback rather than unsafe concurrency. Accepted children pass a durable `IntegrationBarrier` and are composed in deterministic dependency order into a separately verified aggregate candidate before final integration.

Directory mutation snapshots the bounded workspace, rejects symlinks, validates changed components, materializes the immutable candidate separately for independent verification, rejects primary-workspace drift, applies only authorized paths, rolls back on failure, and proves the resulting tree identity. Parallel directory mutation remains intentionally withheld until a separately certified aggregate backend exists.

Programmatic callers can create `medusa_runtime::workspace::Workspace::ephemeral()` for scratch artifacts without starting from a repository. Cleanup is explicit and can only delete a Medusa-owned ephemeral root.

Git is not required for documentation, analysis, supplied-source research, reports, or other bounded artifact work. A non-Git workspace does **not** grant ambient network/browser capabilities: external research still depends on explicitly supported and policy-authorized integrations. Model-executable browser actions remain quarantined.

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

Supported runtime configuration currently includes:

```toml
version = 1

[agent]
mode = "yolo"
max_turns = 500
parallel_workers = 4

[model]
provider = "minimax"
fallback_providers = []
role_routes = {}
name = "MiniMax-M3"
protocol = "openai"
temperature_milli = 200
max_output_tokens = 32768
context_window_tokens = 1000000
auto_compact_percent = 40
auth = "api-key"

[memory]
enabled = true
format = "markdown"

[verification]
required = true
browser_on_ui_change = true
```

`agent.parallel_workers` controls bounded parallel **tool work** in configuration schema v1. It does not authorize autonomous agent recursion and is not the Git mutation-DAG child limit. The current conflict-aware mutation implementation has a separate hard safety bound of three mutating children and activates only after typed risk/confidence/scope/resource checks.

Command-line overrides use `--set key=value`:

```bash
medusa --set agent.mode=read-only
medusa --set agent.max_turns=100
medusa --set verification.browser_on_ui_change=false
```

Configuration commands include `medusa config init`, `show`, `edit`, `get`, `set`, `unset`, `validate`, `doctor`, `reset`, and named profile management. See [Configuration](docs/CONFIGURATION.md) for the canonical schema and migration notes.

## Capabilities and strengths

The 10 capabilities below are recorded as `production` maturity in [`docs/CAPABILITY-CLAIMS.json`](docs/CAPABILITY-CLAIMS.json). Each claim links to owners, production paths, tests, gates, entrypoints, platforms, and promotion criteria.

| # | Capability | Maturity |
|---|---|---|
| 1 | Shared runtime — TUI, desktop, and headless interfaces share one frontend-neutral runtime. | production |
| 2 | Durable sessions & memory — sessions, prompts, memory, provenance, lifecycle, recall. | production |
| 3 | GitHub service — guarded auth and repository workflow operations through a service boundary. | production |
| 4 | Provider context resilience — config, retries, failover, capability authority, context accounting, compaction. | production |
| 5 | Identity, approval, transactions — exact-action approvals, transaction rollback, durable decisions. | production |
| 6 | Daemon — bounded concurrency, reconnect, cancellation, process-tree termination, graceful drain, recovery. | production |
| 7 | Release trust — validated artifacts, checksums, SBOMs, provenance attestations, draft-only publication. | production |
| 8 | Self-update — verified immutable-main updates that respect package-manager ownership. | production |
| 9 | Multi-agent execution — read-only planner/risk teammates, conflict-aware bounded Git parallel implementers, and isolated directory mutation under one transaction authority. | production |
| 10 | Truthful code-intelligence levels — per-language semantic depth, repository-scoped TypeScript/JavaScript semantics, guarded rename. | production |

### Workspace intelligence

- workspace and file discovery;
- bounded text and symbol search;
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
- durable worker leases, epochs, task evidence, child acceptance, aggregate barriers, and cleanup;
- dedicated zero-tool parent review, independent verification, authorization, guarded integration, and reconciliation;
- authoritative primary workspace verification gate.

### Tools and integrations

- guarded workspace file operations;
- Git-aware change and integration workflows where Git is present;
- policy-controlled command execution;
- required UI-change browser verification through the internal sidecar, while model-executable browser actions remain quarantined;
- image/file prompt attachments when provider capabilities permit;
- MCP and extension boundaries;
- provider routing/fallback chains;
- GitHub, update, and optional Desktop Commander integrations.

### Verification

After mutation, Medusa can inspect typed changed components, select impacted checks, run broader checks when narrow selection would be unsafe, require visible UI evidence for effective interface changes, record evidence/overrides/results, and reject completion when required evidence is absent or failed. Directory artifacts without a declared project-level verification command are reported truthfully rather than pretending a command ran; candidate identity, scope, review, independent verification, integration, and resulting tree identity are still enforced.

### Memory and learning

Medusa supports workspace-scoped Markdown memory, bounded recall with provenance, memory consolidation/writeback, verified-session learning/probationary lessons, failure history/negative outcomes, and a rule that optimistic or unverified completion cannot become accepted positive experience.

### Observability and resilience

Typed runtime events cover usage, progress, activity, team, plan, question, completion, cancellation, failure, and recovery state. Process registry/supervision, checkpoints, replay, time travel, transactions, continuity, deterministic cancellation/resource cleanup, and privacy-safe diagnostics provide the recovery foundation.

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

### Major layers

| Layer | Responsibilities | Principal crates |
|---|---|---|
| **Interfaces** | CLI parsing, terminal interaction, desktop UI, Telegram command/rendering | `medusa-cli`, `medusa-tui`, `apps/medusa-desktop`, daemon Telegram modules |
| **Runtime authority** | Session lifecycle, commands, events, coordination, completion, cancellation | `medusa-runtime`, `medusa-agent`, `medusa-daemon` |
| **Multi-agent execution** | Task contracts, scheduling, leases, mutation DAGs, isolated implementation, barriers, parent review | `medusa-multi-agent-scheduler`, `medusa-workers`, `medusa-worker-leases`, runtime coordinators |
| **Context and intelligence** | Workspace context, retrieval, turn assembly, goals, progress, confidence, failure | context and intelligence crate families |
| **Tools and policy** | Capability discovery, authorization, execution control, Git/browser/extensions | capability, policy, control, extension, GitHub, and browser crates |
| **State and recovery** | Sessions, checkpoints, replay, time travel, continuity, transactions, recovery | checkpoint, replay, time-travel, continuity, transaction, recovery crates |
| **Memory and improvement** | Markdown memory, consolidation, writeback, learning, hardening | memory, improvement, and hardening crate families |
| **Containment** | Platform sandboxing, process ownership, limits, cleanup | `medusa-process-containment`, `medusa-process-registry`, `medusa-runtime-supervisor` |
| **Protocol and providers** | Typed frontend/event contracts, model routes, Realtime voice contracts | `medusa-protocol`, `medusa-provider`, `medusa-openai-realtime` |

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

### Approvals

Approvals bind to structured actions and current runtime state. Exact command allowlists, interactive approve-once decisions, Telegram callback foundations, expiry, idempotency, and plan fingerprints do not weaken policy or containment.

### Cancellation and cleanup

Cancellation propagates through runtime, model, tool, process, worker, transaction, and frontend state. Process ownership and containment terminate child process trees and preserve durable cancellation/failure evidence.

## Persistent state and recovery

Workspace-local state lives under `.medusa`. Durable authority categories include sessions/objectives/events, plans/contracts/questions/approvals, provider/tool/integration/verification evidence, worker leases/epochs/isolated candidates, Git commit or directory snapshot receipts, checkpoints/replay/time-travel, transaction/review/authorization/rollback records, failure/recovery decisions, memory/learning, and frontend continuity.

Resume and recovery never treat display text or an optimistic model response as authoritative execution evidence. Schema names such as `prepared_commit` and `prepared_tree` are retained for compatibility; in directory workspaces they hold content-addressed snapshot/tree identifiers rather than Git object IDs.

## Platform support

Canonical workflows test the Rust workspace and daemon behavior across Linux, macOS, and Windows. Desktop CI builds and validates unsigned packages on all three platforms. Parallel Mutation Certification proves the Git multi-implementer path cross-platform; workspace-backend tests cover non-Git isolation and drift-safe integration.

Repository gates cover formatting, Clippy, tests, documentation, dependency/security policy, architecture drift, containment regressions, adversarial cases, fuzz smoke tests, migration/chaos recovery, package smoke tests, and selected live-provider scenarios.

Platform support does not imply identical containment, audio, browser, credential-store, or operating-system signing behavior.

## Current limitations

- Autonomous nested delegation, unconstrained dynamic agent teams, consensus voting, and distributed multi-host mutation transactions are not supported.
- Conflict-aware parallel **mutation** currently requires a Git workspace; directory/ephemeral workspaces deliberately use one isolated snapshot implementer.
- Directory mutation fails closed on symlink-bearing workspaces; use Git mutation when symlink semantics must be preserved.
- OpenAI Realtime and Telegram end-to-end acceptance still require real ChatGPT OAuth, audio hardware, bot/chat/Mini App access, and sanitized evidence.
- ChatGPT OAuth depends on the separately distributed `openai-oauth` gateway and Node.js.
- Browser crates remain quarantined from advertised executable model actions until dispatcher, permission, and authenticated behavioral evidence are certified.
- Native Anthropic-compatible provider requests are currently non-streaming even though streaming is represented in capability contracts.
- Screenshot input is accepted only when the selected provider declares compatible image support and limits.
- Desktop release packages are unsigned at the operating-system level.
- Windows command containment requires the Windows 11 composable sandbox API.

## Roadmap

Open work is tracked in repository issues. The previous product-presentation, Telegram, durable-journal/continuity, unified-config, and conflict-aware parallel-mutation roadmap items are shipped; parallel mutation issue [#691](https://github.com/benclawbot/Medusa/issues/691) is closed and its production path is guarded by Parallel Mutation Certification.

### Performance — [#684](https://github.com/benclawbot/Medusa/issues/684)

Make Medusa the fastest coding agent measured by time from accepted objective to a correct, independently verified result, not by earliest unverified edit or raw token generation speed. Remaining children include pipeline verification/resource warming (#689), bounded speculative implementation (#690), and durable-journal/runtime hot-path optimization (#692).

### Reliability, privacy, and resilience

- [#776](https://github.com/benclawbot/Medusa/issues/776) Repository-wide fuzz, chaos, and crash-resilience certification.
- [#778](https://github.com/benclawbot/Medusa/issues/778) Production operational reliability, diagnostics, and degraded-mode certification.
- [#777](https://github.com/benclawbot/Medusa/issues/777) Data lifecycle, privacy, retention, redaction, export, and deletion across durable state.

### Long-context delegation, time travel, and memory

- [#758](https://github.com/benclawbot/Medusa/issues/758) Contained persistent analysis workspace with context-as-data and typed recursive delegation.
- [#755](https://github.com/benclawbot/Medusa/issues/755) Provenance-linked semantic summaries for abandoned time-travel branches.
- [#754](https://github.com/benclawbot/Medusa/issues/754) Compaction Manifest V2 with authoritative state, semantic history, and intact recent turns.

Issue #758 does **not** mean autonomous recursive delegation is currently production. The current runtime keeps nested delegation disabled; that issue tracks a separately typed/promotion-gated future capability.

### Skills, refinement, and observability

- [#760](https://github.com/benclawbot/Medusa/issues/760) Typed executable skill packages with contained runners, provenance, and verification.
- [#759](https://github.com/benclawbot/Medusa/issues/759) Evidence-gated continual refinement with immutable policy roots and rollback.
- [#757](https://github.com/benclawbot/Medusa/issues/757) Route scheduled/wakeup prompts through the durable session action plane.
- [#756](https://github.com/benclawbot/Medusa/issues/756) Read-only live-session observer and non-invasive side-question API.

### Manual live acceptance

- [#817](https://github.com/benclawbot/Medusa/issues/817) Production self-improvement loop proof. See [Live self-improvement acceptance](docs/LIVE-SELF-IMPROVEMENT-ACCEPTANCE.md).
- [#719](https://github.com/benclawbot/Medusa/issues/719) OpenAI Realtime voice and Telegram end-to-end proof.

## Project documentation

- [Product architecture](docs/ARCHITECTURE.md)
- [Workspace modes](docs/WORKSPACES.md)
- [Multi-agent execution](docs/MULTI_AGENT_EXECUTION.md)
- [Mutating worktree integration](docs/MUTATING-WORKTREE-INTEGRATION.md)
- [Contributor architecture](docs/CONTRIBUTOR-ARCHITECTURE.md)
- [Production execution trace](docs/PRODUCTION-EXECUTION-TRACE.md)
- [Configuration](docs/CONFIGURATION.md)
- [Capability claims](docs/CAPABILITY-CLAIMS.json)
- [Capability evidence](docs/CAPABILITY-EVIDENCE.md)
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
