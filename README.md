<p align="center">
  <img src="assets/medusa-banner.png" alt="Medusa — Plan, Execute Safely, Recover" width="100%">
</p>

# Medusa

A local-first, workspace-aware agent written in Rust. Medusa can work in Git repositories, ordinary directories, or explicit ephemeral workspaces. It turns objectives into explicit plans, coordinates bounded specialist agents, isolates mutation, runs guarded commands, verifies results, preserves durable evidence, and resumes work across the CLI, terminal UI, desktop app, daemon, and Telegram.

The product model is **Plan, Execute Safely, Recover**:

- **Plan.** An objective and workspace context become explicit task contracts and a reviewable plan.
- **Execute Safely.** Read-only teammates scout the work. Git mutation can use a conflict-aware bounded implementation DAG with isolated worktrees; ordinary directories use one isolated content-addressed snapshot implementer. Review, independent verification, authorization, and integration remain separate runtime authorities.
- **Recover.** Sessions, plans, events, approvals, worker leases, immutable candidates, delegation contracts, agent scopes, effective model-request manifests, transactions, verification, and recovery state live under `.medusa` as durable authority. Interruption, cancellation, or crash never gets rewritten as success.

**Status (v1.0.2, `main`):
- **CLI, TUI, desktop application, daemon, telegram access, shared runtime**
- Bounded multi-agent execution, conflict-aware parallel Git mutation, non-Git directory mutation, platform containment, durable sessions, immutable worker delegation contracts, transactional per-agent scopes, durable worker instruction delivery, effective model-request manifests, deterministic request reconstruction, certified tool execution, verified self-update, and repository-enforced engineering policy are shipped.

The canonical status authorities are `docs/CAPABILITY-CLAIMS.json`, `docs/architecture/baseline.json`, and `docs/provider-support.json`.

The runtime also contains an accepted **transactional component-runtime contract** for safe incremental harness evolution: stable component generations, scoped host context, resource ownership, reversible effect journals, declarative dependencies, committed-versus-target provider views, ordered retirement, versioned desired state with compare-and-swap updates, health-validated replacement, containment-bound capabilities, validated self-modification proposals, explicit external-commit semantics, and deterministic fault/invariant checks. This is an adoption seam, not a claim that every production subsystem has already been migrated to component lifecycle management.

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
- **Immutable delegation and retry authority.** A model-backed worker is bound to a sealed delegation contract before its session is created. Mutating retries are rebuilt from fresh model context and fresh step capsules tied to the prior authority and lineage; a retry may become narrower, but it cannot silently widen its authority.
- **Transactional per-agent authority.** Each live agent session has an explicit durable scope covering repository identity, provider profile, execution policy, effective tools, capability registry state, team/member identity, and cancellation ownership. Scope lifecycle is published before model/tool admission and fails closed when stale or stopped.
- **No recursive swarm authority.** Only the root coordinator creates workers. Implementers cannot spawn more implementers, widen their contracts, or integrate their own work.
- **Safe by default.** Writes are path-checked and transactional. Git workspaces use worktree isolation; directory workspaces use immutable content-addressed snapshots, primary-drift detection, and rollback-protected integration. Commands are policy-checked and execute through platform containment that fails closed when unavailable.
- **Durable and inspectable.** Effective model requests are persisted before provider calls with request/provider/scope/configuration fingerprints, source-event linkage, delivered session actions, compaction provenance, and tool-schema fingerprints. A versioned reconstruction path can independently rebuild model-visible requests from durable sources and detect divergence.
- **Result-authoritative learning.** Behavioral learning uses independently verified root-task outcomes rather than model self-report. Failed, cancelled, partial, censored, and inconclusive runs remain evidence.
- **One runtime, multiple frontends.** CLI, TUI, desktop, daemon clients, and Telegram use the same shared runtime and protocol authorities instead of creating separate agents.
- **Cross-platform Rust core.** The workspace and platform-specific authority paths are tested across Linux, macOS, and Windows.

## Interfaces

The interface changes presentation and interaction style; it does not create a separate policy engine, transcript, workspace authority, provider authority, or scheduler.

| Interface | Status | Best for |
|---|---|---|
| **CLI** | Shipped | Automation, CI/CD, scripts, diagnostics, workspace utilities, headless objectives. |
| **Terminal UI (TUI)** | Shipped | Interactive coding, general chat, documentation, analysis, plans, approvals, activity, sessions, recovery, metrics, keyboard-first workflows. |
| **Desktop application** | Shipped | Graphical workspace with sessions, chat, plans, activity, settings, review, attachments, usage, and voice controls. |
| **Telegram frontend** | Shipped | Remote session attachment, mobile status/control, approvals, progressive rendering, files, voice notes, Mini App voice surface. |
| **Daemon** | Shipped | Bounded concurrency, reconnect, cancel-and-drain, IPC control plane for other clients. |
| **Full-duplex voice** | Shipped | Provider-neutral realtime core; microphone streaming remains gated to an established supported route. |

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

The TUI presents the shared runtime event stream as a conversation and activity timeline. It supports plans, questions, approvals, queued follow-ups, cancellation, session resume, settings, usage metrics, clipboard/file/image attachments, recovery views, team activity, provider/model/effort selection, and realtime voice controls. First-run provider setup and `/settings` use the shared provider/model catalog and revision-aware configuration authority rather than a terminal-only configuration store.

General-chat turns can avoid repository indexing/scanning when the task does not need workspace context. Repository-aware paths still activate the normal workspace intelligence, policy, verification, and durable evidence machinery.

### Desktop application

The desktop app is a Tauri/React shell over the same Medusa runtime. It provides session navigation, a central execution timeline, chat, plan and activity presentation, provider/runtime status, settings, attachments, review and learning surfaces, usage telemetry, and desktop-native voice controls. Guided onboarding and model discovery consume the same canonical provider/model metadata used by the runtime.

### Telegram

Telegram is a frontend to the same authoritative Medusa session, not a separate bot-owned agent. The repository implementation includes progressive rendering, action cards, approvals, durable session attachment/control, files and voice-note handling, and the Mini App voice surface. 

See [Telegram](docs/TELEGRAM.md) for setup, service operation, and Mini App wiring.

### Full-duplex voice

Medusa has one provider-neutral realtime voice model rather than a separate voice agent for each frontend. It includes bounded input/output audio queues, partial/final transcripts, voice activity, tool/approval states, reconnect behavior, deterministic resource cleanup, and barge-in that stops spoken output without implicitly cancelling the coding task.


## Installation

### Prerequisites

- Rust 1.88 or newer for source builds; the repository pins Rust 1.88.0
- A supported model connection for model-dependent work
- The platform containment backend required for guarded shell execution
- Node.js 22 for ChatGPT OAuth, required UI-change browser verification, desktop development, or desktop packaging
- **Git only when needed:** source installation/cloning and Git-backed mutation require Git; packaged Medusa can perform ordinary-directory and ephemeral workspace work without a Git repository

### Install the CLI

The normal install path downloads the current prebuilt release, so it does not compile the Rust workspace. The installer shows download progress and launches Medusa in the same terminal when installation completes.

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

The canonical selectable-route, support-tier, credential, live-dogfood, protocol, and Realtime status matrix is [`docs/provider-support.json`](docs/provider-support.json). Current selectable routes are:

| Route | Support | Protocol | Credential source |
|---|---|---|---|
| MiniMax direct | production-supported | Anthropic Messages-compatible | `MINIMAX_API_KEY` |
| Anthropic | production-supported | Anthropic Messages | `ANTHROPIC_API_KEY` |
| Anthropic-compatible | custom endpoint | Anthropic-compatible | `MEDUSA_API_KEY`, optionally `MEDUSA_BASE_URL` |
| OpenAI API | production-supported | OpenAI-compatible | `OPENAI_API_KEY` |
| ChatGPT OAuth | production-supported | OpenAI-compatible local gateway | `openai-oauth` gateway / existing ChatGPT account state |
| OpenAI-compatible | custom endpoint | OpenAI-compatible | `MEDUSA_API_KEY` plus configured endpoint |
| OmniRoute | managed route | OpenAI-compatible | managed external route |
| Local runtime | local route | OpenAI-compatible | user-operated local runtime |

ChatGPT OAuth is supplied through the separately distributed `openai-oauth` loopback gateway. Medusa can reuse an existing OAuth credential, start or adopt the authenticated local gateway when the route is selected, and validate discovered models without reading the gateway credential file directly. See [ChatGPT OAuth](docs/CHATGPT-OAUTH.md):

```bash
npx --yes openai-oauth@2.0.0 --no-open --detach
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
medusa update
```

`medusa update --check` is read-only. Source-installed binaries update from a verified exact-`main` prebuilt artifact rather than compiling the workspace locally. Rolling publication is revision-bound and atomic: the updater waits through bounded transient publication lag, verifies manifest identity, byte count and SHA-256, replaces atomically, performs health/rollback handling, coordinates daemon handoff, and on Windows restarts in the same console. Package-managed installations are not overwritten and instead report the relevant package-manager command.

## Workspace modes

| Mode | Mutation isolation | Parallel mutating implementers | Acceptance identity |
|---|---|---|---|
| **Git** | Dedicated branch/worktree per implementer | Yes, up to three when the conflict-aware DAG accepts the decomposition | Git commit/tree + typed receipts |
| **Directory** | Isolated content-addressed snapshot copy | No; one isolated implementer | `dir-<sha256>` snapshot/tree + typed receipts |
| **Ephemeral** | Medusa-owned temporary directory using the directory backend | No | Content-addressed snapshot/tree until explicit cleanup |

Git parallel mutation is **not** “agents editing the same checkout.” Every child has exact ownership, its own worktree, an immutable delegation contract, an agent scope, independent scope/verification evidence, and no integration authority. Conflicts across manifests, lockfiles, migrations, snapshots, generated outputs, dependencies, or path ownership create ordering/fallback rather than unsafe concurrency. Accepted children pass a durable `IntegrationBarrier` and are composed in deterministic dependency order into a separately verified aggregate candidate before final integration.

Directory mutation snapshots the bounded workspace, rejects symlinks, validates changed components, materializes the immutable candidate separately for independent verification, rejects primary-workspace drift, applies only authorized paths, rolls back on failure, and proves the resulting tree identity. Parallel directory mutation remains intentionally withheld until a separately certified aggregate backend exists.

Programmatic callers can create `medusa_runtime::workspace::Workspace::ephemeral()` for scratch artifacts without starting from a repository. Cleanup is explicit and can only delete a Medusa-owned ephemeral root.

Git is not required for documentation, analysis, supplied-source research, reports, or other bounded artifact work. A non-Git workspace does **not** grant ambient network/browser capabilities: external research still depends on explicitly supported and policy-authorized integrations.

See [Workspace modes](docs/WORKSPACES.md) and [Multi-agent execution](docs/MULTI_AGENT_EXECUTION.md) for the exact safety rules.

## Configuration

Medusa has two related configuration layers:

1. the established typed product configuration (`Config`) used for provider, agent, memory, verification, and other product settings; and
2. a versioned, fingerprinted runtime-loop configuration contract for tunable orchestration behavior, provenance, replay, and preflight validation.

Unknown protected fields fail closed. Configuration remains an input to fixed Medusa authorities; it cannot replace or weaken capability, containment, approval, mutation, verification, integration, journal, or evaluator authority.

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
protocol = "anthropic"
auth = "api-key"
streaming = true

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

Configuration commands include `medusa config init`, `show`, `edit`, `get`, `set`, `unset`, `validate`, `doctor`, `reset`, and named profile management. Interactive TUI and Desktop configuration consume the same provider catalog/model registry and revisioned profile authority.

The runtime-loop contract validates combinations before provider/tool side effects, fingerprints the effective behavior, records value provenance, rejects unavailable Code Mode/service routes, validates budgets and route admission, freezes effective behavior for resume/replay where required, and exposes explainable configuration state. See [Configuration](docs/CONFIGURATION.md) for the canonical schema and migration notes.

## Capabilities and strengths

The production capability claims are recorded in [`docs/CAPABILITY-CLAIMS.json`](docs/CAPABILITY-CLAIMS.json). The current production set includes:

| Capability | Maturity |
|---|---|
| Shared runtime across TUI, desktop, and headless entrypoints | production |
| Durable sessions, prompts, memory, provenance, lifecycle, and recall | production |
| Guarded GitHub service boundary | production |
| Provider configuration, role routing, retries/failover, context accounting, compaction, and reasoning exchange | production |
| Identity, exact-action approvals, parent review, independent verification, integration authorization, rollback, and durable decisions | production |
| Daemon concurrency, reconnect, cancellation, process-tree termination, graceful drain, and recovery | production |
| Release trust: validated artifacts, checksums, SBOMs, provenance, draft-only publication | production |
| Verified immutable-`main` self-update | production |
| Bounded multi-agent execution with conflict-aware Git mutation and isolated non-Git mutation | production |
| Truthful per-language code-intelligence levels and guarded rename | production |

### Workspace intelligence

- workspace and file discovery;
- bounded text and symbol search;
- evidence-ranked repository snapshots rebuilt from current repository state for coding turns;
- durable coding trajectory across compaction/resume, preserving objective, constraints, relevant/modified paths, decisions, verification requirements, failures, blockers, external evidence, and repository identity while invalidating stale assumptions on drift;
- structured repair ledgers that aggregate diagnostics, preserve exact expansion references, link common-root failures, and track repair attempts and resolution state;
- bounded adaptive roadblock recovery that suppresses equivalent failed strategies and escalates when admissible transitions are exhausted;
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
- fresh-context **step capsules** for mutating retries, bound to prior authority, lineage, evidence, and remaining work rather than blindly replaying stale model context;
- durable worker/team instructions admitted through session delivery semantics rather than a separate model-context mailbox authority;
- effective model-request manifests persisted before provider calls, including request/provider/scope/configuration fingerprints, source-event linkage, delivered session actions, compaction provenance, and tool-schema fingerprints;
- deterministic request reconstruction from durable named source records, with divergence detection against the exact frozen pre-dispatch request artifact;
- bounded speculative implementation for eligible resolved-scope work, where isolated preparation may overlap upstream review but promotion still fails closed on exact assumptions and current authority;
- shared semantic execution reporting for consistent frontend projections;
- durable worker leases, epochs, task evidence, child acceptance, aggregate barriers, and cleanup;
- dedicated zero-tool parent review, independent verification, authorization, guarded integration, and reconciliation;
- authoritative primary-workspace verification gate.

### Transactional component runtime

`medusa-runtime::component_runtime` is the accepted reference contract for safe component lifecycle and incremental self-evolution. It currently provides:

- stable logical component IDs and monotonic generations;
- explicit lifecycle state and scoped host context;
- resource/registration attribution to exact component generations;
- reverse-order, idempotent reversible effect journals with inspectable cleanup debt;
- declarative `requires`, `provides`, and host-capability specifications;
- deterministic dependency resolution, ambiguity/cycle rejection, and committed-versus-target dependency views;
- consumer-before-provider retirement with explicit blocked-retirement state;
- authoritative versioned desired state with compare-and-swap updates and idempotency records;
- health-validated candidate replacement that keeps the previous healthy generation available on failure;
- one normalized capability intent feeding host authority and containment policy construction where supported;
- self-modification through typed, validated desired-state proposals with source provenance and stale-conflict handling rather than direct agent registry mutation;
- explicit separation of reversible Medusa-owned effects from irreversible/external commits with idempotency, uncertain-commit, and compensation-required states;
- deterministic fault injection and runtime invariant checks for lifecycle, ownership, dependency, capability, and recovery boundaries.

This contract is intentionally being adopted incrementally. Its presence does not mean all existing runtime services are dynamically replaceable, and fixed Medusa authorities remain non-pluggable.

### Provider routing and context

Provider selection is explicit and role-aware. `model.role_routes` can pin planner, implementer, reviewer, repair, summarization, or formatting phases to configured primary/fallback profiles without silently replacing a user-pinned route. Cross-model context uses provider-neutral bounded reasoning exchange; provider-native continuation state remains separately bound to its exact provider/protocol/route/model/session and fails closed when incompatible.

When no user pin overrides route choice, the provider manager can use durable route telemetry for latency, throughput, retry recovery, error categories, downstream verified success, and externally supplied monetary cost. Bounded hedging is eligibility- and budget-gated, publishes only one authoritative winner, and preserves ordinary retry/failover when a hedge is inadmissible or unsuccessful.

Model context windows are resolved by `(provider, model)` for fixed-vendor catalog entries. Unknown/custom routes retain their configured limits rather than inheriting a similarly named vendor model's limit.

Provider HTTP handling bounds retained error bodies and successful JSON response sizes. Shell/provider evidence paths preserve privacy boundaries; provider-required opaque reasoning history may be retained transiently for wire continuity without becoming normal user-visible or durable reasoning content.

### Tools and integrations

- guarded workspace file operations;
- Git-aware change and integration workflows where Git is present;
- policy-controlled command execution;
- a certified typed tool-execution lifecycle with fixed stages, monotonic guard denials, explicit approval state, cancellation normalization, deterministic input fingerprints, and immutable resolved-handler identity;
- canonical typed tool-result foundations with explicit consumer projections; shell expansion artifacts are content-addressed and tamper-checked, while model-visible output remains bounded and redacted;
- typed executable skill packages with declared capabilities, repository/network scope, side effects, artifacts, typed JSON I/O, resource budgets, digest-bound validation receipts, contained execution, and fail-closed stale/missing validation;
- a contained per-session analysis workspace for bounded persistent analytical state and fixed-reducer execution without granting repository mutation, credentials, ambient network, or independent provider authority;
- safe non-authority service/provider seams where implementation substitution is allowed without making policy, verification, journal, approval, containment, or integration authorities pluggable;
- validated project plugin metadata discovery that does not itself grant executable authority;
- required UI-change browser verification through the internal sidecar; model browser actions remain readiness-gated explicit opt-in preview;
- image/file prompt attachments when provider capabilities permit;
- MCP and extension boundaries;
- provider routing/fallback chains;
- GitHub, update, and optional Desktop Commander integrations.

### Verification

After mutation, Medusa can inspect typed changed components, select impacted checks, run broader checks when narrow selection would be unsafe, require visible UI evidence for effective interface changes, record evidence/overrides/results, and reject completion when required evidence is absent or failed. A durable verification-completion contract binds required evidence to exact repository state, invalidates scoped evidence after mutation, preserves explicit unavailable/waived/superseded dispositions, and blocks completion until required evidence is fresh.

Authoritative verification executes dependency-aware DAG waves, persists restart-safe checkpoints, reuses prior receipts and warm resources only when complete input/provenance identity still matches, cancels obsolete process trees on repository drift, and retries against refreshed state rather than accepting stale results. Runtime-owned `.medusa` state is excluded from product-source drift fingerprints where appropriate so verification activity does not invalidate itself, while genuine product drift still fails closed.

Live coding evidence preserves committed product diffs, not only dirty working-tree state. Verification/tooling byproducts such as narrowly defined npm log residue are separated from product mutation scope without weakening arbitrary out-of-scope write enforcement.

### Memory and learning

Medusa supports workspace-scoped Markdown memory, bounded recall with provenance, memory consolidation/writeback, verified-session learning/probationary lessons, failure history/negative outcomes, and a rule that optimistic or unverified completion cannot become accepted positive experience.

Continual refinement is evidence-gated: typed proposals preserve provenance, deterministic evaluation and explicit approval precede activation, security/authority roots stay outside refinable content, activation history is append-only, and exact rollback is retained. User corrections and accepted runtime signals feed a privacy-filtered typed provenance graph and evaluated correction loop rather than self-activating directly.

Production behavioral learning follows one result-authoritative lifecycle:

**execute -> independently verify -> record outcome -> compare comparable cohorts -> detect improvement/regression -> test bounded adaptation -> independently re-verify -> promote or roll back**

Model or worker claims such as “fixed,” “tests pass,” or confidence are observations, not correctness authority; independently verified root-task outcomes are the ground truth. Failed, cancelled, partial, censored, and inconclusive runs remain evidence, and monetary cost remains unknown unless an authoritative cost observation exists.

The repository ships typed foundations for canonical behavioral outcomes, replayable/concurrency-safe learning projections, task-aware cohorts, drift reporting, bounded adaptive policy/controller contracts, Code Mode presentation, canonical tool results, model-experience/cache accounting, and runtime-loop configuration. README wording intentionally does **not** claim full end-to-end autonomous behavioral optimization acceptance until the corresponding production/live evidence is complete.

### Engineering policy

Critical engineering rules are machine-checkable rather than only prose conventions. The repository policy covers protected authority paths, crate/dependency boundaries, unsafe/FFI locations, generated/source-of-truth synchronization, documentation and capability claims, required platform/provider checks, and canonical truth-store ownership.

Policy resolution is protected against self-weakening: changes to the policy/evaluator are evaluated against protected base-branch authority. The applicable constraint set can be explained for a change and the same minimum checks are enforced in CI.

### Observability and resilience

Typed runtime events cover usage, progress, activity, team, plan, question, completion, cancellation, failure, and recovery state. Shared execution reporting collapses low-level activity into deterministic semantic updates across Headless, TUI, Desktop, Telegram, and future frontends. Read-only live-session observation reconstructs bounded stage/plan/tool/file/verification/blocker state from the durable journal, and side questions over that snapshot cannot steer, mutate, approve, or cancel the primary run.

Scheduled timer/heartbeat/file/process/external-signal wakeups enter the same durable session-action authority with idempotent occurrence provenance and explicit busy-session semantics instead of creating a scheduler-owned prompt queue. Process registry/supervision, generation-bound process identity, checkpoints, replay, time travel, transactions, continuity, deterministic cancellation/resource cleanup, operational health/support bundles, repository-wide lifecycle/privacy certification, and resilience fault campaigns provide the recovery foundation.

## Architecture

<p align="center">
  <img src="docs/assets/medusa-architecture.jpg" alt="Medusa architecture: interfaces, shared runtime, multi-agent execution, tools and policy, state and recovery, memory and learning, containment, and a shared authoritative data layer" width="100%">
</p>

The canonical coding path is:

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
  -> build or rebuild fresh bounded model context
  -> persist effective model-request manifest before provider call
  -> execute the provider attempt under the bound scope/request authority
  -> on mutating retry: create a fresh StepCapsule bound to prior authority/lineage
```

For component lifecycle and controlled self-evolution, the reference contract is separate from the coding-session orchestration path:

```text
versioned Desired Component State
  -> validate graph / dependencies / capabilities
  -> compare-and-swap commit
  -> Reconciler plan
  -> ComponentRuntime generations
       -> scoped context + resource ownership
       -> committed/target dependency views
       -> reversible EffectJournal
       -> health-validated replacement
       -> ordered retirement
       -> ExternalCommitLedger for irreversible effects
  -> deterministic invariant/fault evidence
```

The component contract is an incremental adoption seam. `RuntimeController`, capability readiness truth, execution policy, approval authority, containment, mutation provenance, parent review, verification, integration, journal authority, and protected evaluator authority are not made dynamically replaceable by this mechanism.

### Major layers

| Layer | Responsibilities | Principal crates |
|---|---|---|
| **Interfaces** | CLI parsing, terminal interaction, desktop UI, Telegram command/rendering | `medusa-cli`, `medusa-tui`, `apps/medusa-desktop`, daemon Telegram modules |
| **Runtime authority** | Session lifecycle, commands, events, coordination, completion, cancellation, agent scopes, component lifecycle contract | `medusa-runtime`, `medusa-agent`, `medusa-daemon` |
| **Multi-agent execution** | Task contracts, immutable delegation, scheduling, leases, mutation DAGs, isolated implementation, barriers, parent review | `medusa-multi-agent-scheduler`, `medusa-workers`, `medusa-worker-leases`, runtime coordinators |
| **Context and intelligence** | Workspace context, retrieval, turn assembly, goals, progress, confidence, failure | context and intelligence crate families |
| **Tools and policy** | Capability discovery, authorization, certified execution, Git/browser/extensions, engineering policy | capability, policy, control, extension, GitHub, and browser crates |
| **State and recovery** | Sessions, request manifests, checkpoints, replay, time travel, continuity, transactions, recovery | agent/session, checkpoint, replay, time-travel, continuity, transaction, recovery crates |
| **Memory and improvement** | Markdown memory, learning, behavioral outcomes/cohorts, refinement monitoring, hardening | memory, improvement, and hardening crate families |
| **Containment** | Platform sandboxing, process ownership, limits, cleanup | `medusa-process-containment`, `medusa-process-registry`, `medusa-runtime-supervisor` |
| **Protocol and providers** | Typed frontend/event contracts, model routes, role routing, reasoning exchange, streaming, Realtime voice contracts | `medusa-protocol`, `medusa-provider`, `medusa-openai-realtime` |

For source-level ownership, see [Product architecture](docs/ARCHITECTURE.md), [Production execution trace](docs/PRODUCTION-EXECUTION-TRACE.md), [Contributor architecture](docs/CONTRIBUTOR-ARCHITECTURE.md), [Workspace modes](docs/WORKSPACES.md), and [ADR-0011: transactional component runtime](docs/architecture/decisions/0011-transactional-component-runtime.md).

## Safety and containment

Medusa is intentionally not an unrestricted shell replacement.

### Workspace writes

Writes resolve against the selected workspace and remain policy checked, transactional, and evidence-bearing. Git mutation preserves symlink semantics through Git/worktree isolation. Directory mutation fails closed on symlinks rather than copying an ambiguous filesystem graph. Sensitive locations remain denied, including `.git` internals, credential stores, operating-system configuration/executable paths, and login-persistence locations.

Repository mutation commits are serialized per repository across the relevant mutation paths, use transaction-unique staging state, and revalidate expected preimages before replacement. Concurrent jobs cannot silently consume each other's staging files or overwrite product bytes without a detected conflict.

### Command containment

Shell execution fails closed if the platform backend is unavailable:

- **Linux:** Bubblewrap with repository access plus explicitly required runtime/toolchain roots, network namespace isolation, and a rebuilt minimal environment.
- **macOS:** Seatbelt with explicit repository/runtime roots, network denial, and a rebuilt minimal environment rather than ambient host-wide file-read access.
- **Windows:** Windows 11 composable sandbox API / AppContainer-style execution with repository read/write binding, selected toolchain roots read-only, network denial, environment allowlisting, suspended process creation before Job Object assignment, process-tree termination, active-process limits, memory limits, and bounded execution.

Windows command containment requires Windows 11 with `Experimental_CreateProcessInSandbox` available. There is no unsandboxed fallback through that API.

### Credentials and sensitive state

Provider credentials are bound to trusted endpoint provenance. Repository configuration cannot silently redirect ambient host credentials to arbitrary origins; remote custom endpoints require HTTPS, loopback HTTP requires explicit development opt-in, and URLs with embedded credentials are rejected.

Sensitive `.medusa` state is persisted behind restrictive permissions: owner-only file/directory modes on Unix and current-user-only ACL handling on Windows. Existing broad permissions are repaired where supported. Shell command/output persistence applies centralized credential redaction before durable lower-trust projections.

### Release and CI trust

Primary and recovery release-signing authorities are isolated into separate protected workflows/environments; signer jobs consume verified immutable artifacts rather than executing release-candidate repository code with both secrets available. External GitHub Actions references are pinned by immutable SHA and repository policy rejects mutable third-party action tags unless explicitly allowlisted.

### Agent, component, and worker authority

Agent scope and delegation are explicit runtime authority, not prompt convention. A scope binds the live session to repository identity, provider profile, execution policy, capability registry fingerprint, effective tools, team/member identity, and cancellation ownership. Worker delegation additionally seals task/lease/repository/worktree/read-write/tool/model/budget/evidence authority.

Component-scoped host context is similarly explicit: a component generation receives only declared host capabilities, resource ownership is attributable to that generation, and unsupported requested containment guarantees fail closed rather than silently downgrading. Self-modifying agents submit typed desired-state proposals; they do not directly mutate lifecycle registries or privileged capability policy.

### Approvals

Approvals bind to structured actions and current runtime state. Exact command allowlists, interactive approve-once decisions, Telegram callback foundations, expiry, idempotency, and plan fingerprints do not weaken policy or containment.

### Cancellation and cleanup

Cancellation propagates through runtime, model, tool, process, worker, transaction, component candidate validation, and frontend state. Process ownership and containment terminate child process trees and preserve durable cancellation/failure evidence. Component cleanup failures remain explicit cleanup debt/blocked-retirement state instead of being discarded.

## Persistent state and recovery

Workspace-local state lives under `.medusa`. Durable authority or rebuildable projections include:

- sessions, objectives, transcript/events, plans, task contracts, questions, and approvals;
- provider/tool/integration/verification evidence;
- effective model-request content/manifests, configuration fingerprints, provider-attempt lineage, and reconstruction receipts;
- worker/team session actions and model-visibility linkage;
- immutable delegation contracts, retry/attempt bindings, and fresh step-capsule lineage;
- transactional agent-scope contracts, generations, lifecycle, revocations, and owned-resource state;
- coding trajectory checkpoints, structured repair ledgers, compaction manifests, and fingerprint-bound advisory summaries;
- verification DAG checkpoints, exact-state reusable receipts, warm-resource descriptors, and repository-drift invalidation evidence;
- continual-refinement proposals/activation history, correction-loop episodes, privacy-filtered provenance/effectiveness evidence, and rollback state;
- canonical behavioral outcomes and rebuildable learning/cohort/drift projections where the corresponding contracts are active;
- scheduled trigger occurrence/dispatch provenance admitted into durable session actions;
- worker leases, epochs, isolated candidates, Git commit or directory snapshot receipts;
- checkpoints, replay, time travel, transaction/review/authorization/rollback records;
- failure/recovery decisions, memory/learning, and frontend continuity.

The transactional component-runtime desired state, proposal records, effect ownership, cleanup debt, external-commit state, and reconciliation evidence use explicit version/revision semantics. The component runtime is an adoption seam and must not be confused with a second session journal or a replacement for existing canonical runtime authorities.

Resume and recovery never treat display text or an optimistic model response as authoritative execution evidence. Model-visible worker instructions are tied to durable session/action state and effective request evidence rather than a standalone mailbox boolean.

## Platform support

Canonical workflows test the Rust workspace and daemon behavior across Linux, macOS, and Windows. Desktop CI builds and validates unsigned packages on all three platforms. Parallel Mutation Certification proves the Git multi-implementer path cross-platform; workspace-backend tests cover non-Git isolation and drift-safe integration.

Repository gates cover formatting, production lint, changed-package tests, documentation, dependency/security policy, architecture and engineering-policy drift, containment regressions, adversarial cases, migration/recovery checks, package smoke tests, selected live-provider scenarios, and path-triggered specialized certifications. Exhaustive all-target/workspace validation remains available in deeper nightly/manual/release contexts rather than being duplicated after every merge.

Platform support does not imply identical containment, audio, browser, credential-store, or operating-system signing behavior.

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
- [Transactional component runtime decision](docs/architecture/decisions/0011-transactional-component-runtime.md)
- [Typed non-authority service-provider decision](docs/architecture/decisions/0010-typed-non-authority-service-providers.md)
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

Use the pinned toolchain and run the gates relevant to the change. For normal Rust PRs the repository CI determines the affected Rust packages and runs the merge-time minimum set; deeper all-target/workspace validation remains available through the repository's deep-validation/release workflows.

Useful local checks include:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo doc --workspace --all-features --no-deps
```

Architecture, capability, documentation, or protected-policy changes should also run the repository-owned checks relevant to the affected paths, including:

```bash
python3 scripts/check-product-architecture.py
python3 scripts/check-capability-evidence.py
python3 scripts/check-documentation.py
python3 scripts/engineering-policy.py explain --help
```

The engineering-policy evaluator is the authority for which additional platform, containment, provider, release, schema/replay, tool-pipeline, benchmark, or protected-boundary checks a change triggers. A patch may request additional validation but may not downgrade the policy-derived minimum.

## License

MIT.
