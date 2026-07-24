<p align="center">
  <img src="assets/medusa-banner.png" alt="Medusa — The Self-Improving Coding Agent" width="100%">
</p>

# Medusa

Medusa is a production-grade autonomous coding agent written in Rust. It combines an interactive terminal, a shared React/Tauri desktop runtime, repository-aware tools, durable Markdown memory, guarded execution, persistent background jobs, multi-worker coordination, browser verification, and release-grade validation.

## Highlights

- **Interactive by default** — run `medusa` in a repository to open the terminal interface.
- **Autonomous coding loop** — inspect, plan, edit, verify, and iterate until completion evidence is available.
- **Minimal coding by default** — every implementation turn follows a native decision ladder that favors reuse, standard and platform capabilities, the smallest correct diff, root-cause fixes, and explicit dependency justification.
- **Shared frontend runtime** — the TUI and Zeus-derived desktop entry point use the same `medusa-runtime` controller instead of separate agent stacks.
- **Validated desktop packages** — CI builds unsigned Linux DEB/AppImage, macOS app/DMG, and Windows NSIS artifacts with synchronized version metadata and SHA-256 evidence.
- **Attested draft releases** — pushed version tags build CLI and desktop assets on all three platforms, generate deterministic CycloneDX and SHA-256 evidence, attach GitHub/Sigstore provenance, and create a draft release without automatic publication.
- **Visible user conversation** — user prompts, assistant responses, Markdown, tool activity, questions, and queued follow-ups remain in one transcript.
- **Mid-turn guidance** — submit extra detail while Medusa is working; it is preserved immediately and injected at the next safe agent-turn boundary.
- **Clipboard-native input** — paste text or screenshots with `Ctrl+V`; supported providers receive screenshots as image context.
- **Repository-aware tooling** — bounded file access, search, atomic writes, patch transactions, shell execution, Git checkpoints, and targeted verification.
- **Persistent background jobs** — bounded daemon workers and queues, overload backpressure, race-safe cancellation, graceful draining, descendant-safe forced shutdown, reconnect, and restart recovery on Linux, macOS, and Windows.
- **Browser verification** — a persistent Playwright sidecar can navigate, click, fill, press, capture screenshots, evaluate JavaScript, and manage tabs.
- **Persistent memory** — Markdown-first storage with validation, indexing, retrieval, lifecycle management, and provenance controls.
- **Parallel workers** — isolated worktrees, deterministic merge behavior, conflict detection, and cleanup safeguards.
- **Extensions and MCP** — skills, hooks, MCP isolation, optional Desktop Commander integration, redaction, and checksummed provenance.
- **Production hardening** — panic-free production targets, source-size and workflow guardrails, dependency metrics, migrations, rollback evidence, fuzzing, chaos recovery, security checks, cross-platform packages, and live-provider validation.

## Minimal coding philosophy

Medusa is designed to produce the **smallest correct change**, not the largest generated solution. For every non-read-only coding turn, the agent receives an always-on implementation policy before it plans or edits code. The default policy level is `full`.

Medusa stops at the first applicable option in this order:

1. Do not implement speculative or unnecessary functionality.
2. Reuse an existing repository helper, type, component, command, or established pattern.
3. Prefer the language standard library.
4. Prefer a native platform, browser, operating-system, database, or framework capability.
5. Reuse an already-installed dependency.
6. Use a direct expression when it remains clear and correct.
7. Otherwise, implement the smallest complete solution.

Before choosing, Medusa inspects the affected flow and relevant callers. Bug fixes should repair the shared root cause once rather than accumulate symptom guards. New abstractions, wrappers, configuration, scaffolding, and dependencies must earn their place; consolidation and deletion are preferred when they leave the code clearer and complete.

Minimalism never overrides correctness or required safeguards. Medusa must preserve security controls, trust-boundary validation, accessibility, data integrity, loss-preventing error handling, concurrency correctness, compatibility requirements, and anything explicitly requested. It must not weaken or rewrite tests merely to hide a broken implementation. Tests change only when intended behavior changes or new behavior needs coverage, and the smallest relevant verification should always be run.

| Typical coding-agent tendency | Medusa default |
|---|---|
| Generate a fresh solution immediately | Inspect and reuse the repository first |
| Add abstractions for possible future needs | Implement only demonstrated requirements |
| Add a dependency for convenience | Prefer stdlib, native capabilities, and installed dependencies |
| Patch each visible symptom | Trace and fix the shared root cause |
| Touch broad areas to make the design “cleaner” | Prefer the fewest files and shortest correct diff |
| Change tests until CI passes | Preserve the intended contract and fix product code |

The policy can be overridden for a process with `MEDUSA_CODING_POLICY`:

```bash
export MEDUSA_CODING_POLICY=full
medusa
```

Supported values are:

| Value | Behavior |
|---|---|
| `off` | Do not inject the minimal coding policy. |
| `lite` | Build the requested change, while briefly surfacing a materially simpler alternative when one exists. |
| `full` | Default. Enforce the complete decision ladder and prefer the shortest correct diff with the fewest touched files. |
| `ultra` | Apply strict YAGNI, challenge speculative requirements, and prefer deletion over addition while still shipping the smallest useful result. |

Read-only sessions do not receive this implementation policy because they cannot change the repository.

## Current status and evidence

- the Rust agent core, TUI, and frontend-neutral runtime
- the Zeus-derived React/Tauri desktop entry point
- durable sessions, prompt drafts, and Markdown memory
- guarded repository tools, browser verification, and parallel workers
- Markdown rendering, user/assistant transcript separation, and mid-turn follow-ups
- optional Desktop Commander MCP integration
- panic-free production targets and least-privilege workflow guards
- cross-platform daemon transport, recovery, and shared TUI/Desktop lifecycle supervision
- bounded daemon workers and queues with explicit overload backpressure
- graceful drain semantics plus race-safe per-job cancellation and immediate process-tree shutdown
- evidence-based dependency pruning with permanent base/current graph metrics
- validated unsigned desktop bundles for Linux, macOS, and Windows with version synchronization and SHA-256 manifests
- a tag-bound, draft-only release workflow with deterministic SBOM/checksum evidence and short-lived OIDC provenance attestations

| Area | Current evidence |
|---|---|
| Interactive product surface | `medusa` launches the TUI; transcript preservation, Markdown rendering, clipboard input, cancellation, metrics, skills, queued follow-ups, questions, plans, and daemon lifecycle transitions are implemented in `medusa-tui`. |
| Agent and repository runtime | `medusa-runtime` owns frontend-neutral interactive session control. Planning, tools, policy, verification, intelligence, memory, and persistence remain implemented across the Rust workspace. |
| Background daemon | `medusa-daemon` provides one durable contract on Linux, macOS, and Windows. It has four fixed workers and a 32-job queue by default, `daemon_busy` backpressure, finite IPC limits, shared frontend supervision, graceful draining, per-job cancellation, and immediate process-tree shutdown. |
| Desktop | `apps/medusa-desktop` adapts the same runtime commands, events, plans, questions, cancellation, follow-ups, skills, provider settings, policy, and daemon lifecycle as the TUI. CI builds and validates unsigned DEB/AppImage, app/DMG, and NSIS artifacts. |
| Dependency hygiene | PR #52 removed five proven-unused direct dependency edges while preserving the resolved package graph. Read-only base/current metrics run in CI. |
| Release publication | A pushed version tag must match synchronized Rust/Tauri/npm metadata, the event SHA, the remote tag target, and `main` ancestry. Read-only platform jobs build the CLI and desktop assets; the final reviewed writer generates `medusa-release-manifest.json`, `SHA256SUMS`, a deterministic CycloneDX SBOM, GitHub/Sigstore attestations, and a draft GitHub Release. It never publishes automatically. |
| Release evidence | `CI`, `Daemon`, `Desktop`, `Refactor Guardrails`, and `Release Gates` enforce formatting, Clippy, panic-free production targets, tests, docs, source-size limits, workflow hygiene, dependency policy, security checks, three-platform integration, desktop bundle validation, coverage, adversarial tests, packages, and live MiniMax scenarios. |

See [Capability evidence](docs/CAPABILITY-EVIDENCE.md) for the auditable mapping from shipped capabilities to code and gates.

## Requirements

- Git
- Rust 1.88 or newer and Cargo; the repository pins Rust 1.88.0
- `MINIMAX_API_KEY` for live MiniMax execution
- Node.js 22 when browser verification, Desktop Commander, desktop development, or desktop packaging is used

## Installation

Install the latest CLI in one line:

```powershell
cargo install --git https://github.com/benclawbot/Medusa.git --locked medusa-cli
```

Installed binaries can check whether they match the current `main` branch without changing the installation:

```text
medusa update --check
```

`medusa update` resolves the current immutable commit on `main`, compares it with the commit embedded in the running binary, and builds `medusa-cli` directly from that branch with Cargo. It requires Cargo and Git access to GitHub. The update runs in a detached helper after the CLI exits; on Windows the helper stops Medusa background processes that use the same executable before Cargo replaces it, then restarts Medusa. Package-managed Linux and macOS installations are not overwritten: Medusa reports the corresponding package-manager command instead. For unattended maintenance, use `medusa update --automatic`.

Set `MEDUSA_UPDATE_POLICY=check` to make a normal `medusa update` report availability only, or `MEDUSA_UPDATE_POLICY=automatic` to permit verified unattended replacement. The command-line `--check` and `--automatic` flags take precedence for a single invocation.

For a development checkout instead:

```bash
git clone https://github.com/benclawbot/Medusa.git
cd Medusa
cargo install --path crates/medusa-cli --locked
medusa --version
medusa doctor
```

`medusa doctor` reports a provider-credential failure until `MINIMAX_API_KEY` is configured. The remaining diagnostics still verify Git, Cargo, repository access, writable state, schema support, and optional integrations.

The release workflow creates a draft only after a version tag is pushed. Draft assets remain unsigned at the operating-system level and require maintainer review before any public publication. See [Release process](docs/RELEASE.md) and [Release compatibility](docs/COMPATIBILITY.md).

### Windows

Install Git, Rustup, and optional Node.js 22 with Winget:

```powershell
winget install --id Git.Git -e --accept-package-agreements --accept-source-agreements
winget install --id Rustlang.Rustup -e --accept-package-agreements --accept-source-agreements
winget install --id OpenJS.NodeJS.22 -e --accept-package-agreements --accept-source-agreements
rustup toolchain install 1.88.0 --profile minimal --component clippy,rustfmt
rustup default 1.88.0
```

Ensure `%USERPROFILE%\.cargo\bin` is on the user `PATH`, then configure the provider credential outside the repository:

```powershell
$env:MINIMAX_API_KEY = '<your-key>'
medusa doctor
```

### macOS and Linux

Install Git and a native build toolchain, then install Rustup and the pinned toolchain:

```bash
source "$HOME/.cargo/env"
rustup toolchain install 1.88.0 --profile minimal --component clippy,rustfmt
rustup default 1.88.0
export MINIMAX_API_KEY='<your-key>'
medusa doctor
```

Debian or Ubuntu users can install the system prerequisites with:

```bash
sudo apt update
sudo apt install -y build-essential git curl pkg-config
```

Do not commit API keys or other credentials.

## Quick start

Open the TUI in the current repository:

```bash
medusa
```

Open another repository or begin with a prepared prompt:

```bash
medusa --repo /path/to/repository
medusa --prompt "Fix the failing tests and verify the result"
```

Resume work:

```bash
medusa --resume <session-id>
medusa --continue
```

### Interactive controls

| Key | Action |
|---|---|
| `Enter` | Submit the prompt, or queue a follow-up while Medusa is working |
| `Shift+Enter` | Insert a new line |
| `Ctrl+V` | Paste clipboard text or attach a screenshot |
| `Ctrl+C` | Cancel the active task; press twice within one second to exit |
| `Ctrl+D` | Exit when the composer is empty |
| `Esc` | Cancel the active task or close the current modal |

Prompt drafts and attachments persist under the repository's `.medusa` directory until submission. Rejected submissions restore the draft. Mid-turn follow-ups remain visible immediately and are applied before the next model turn.

Installed skills are directly invokable by name. Built-in commands take precedence over same-named skills:

```text
/skills
/release
/release prepare version 1.0
/release@user
```

Typing `/` filters built-in commands and installed skills; Tab completes the selected entry. Selected skill instructions are ephemeral system context and are not written into durable session messages.

## Background daemon

`medusa-daemon` owns repository-scoped background jobs, reconnectable IPC, durable job records, process ownership, bounded execution, restart recovery, per-job cancellation, graceful draining, and immediate process-tree shutdown.

- Linux and macOS use `.medusa/daemon/medusa.sock` as a Unix-domain socket.
- Windows uses the same path as an endpoint descriptor containing an ephemeral loopback TCP address; non-loopback descriptors are rejected.
- A new connection is used per request, so clients can disconnect while daemon-owned jobs continue.
- Production defaults are four concurrent workers and 32 queued jobs.
- A full queue returns `daemon_busy`; rejected work does not retain a durable job record.
- Local reads and writes time out after five seconds; requests larger than 64 KiB are rejected.
- Graceful shutdown stops request acceptance, drains queued and running accepted jobs, joins workers, and releases ownership.
- `Cancel { job_id }` removes queued work before execution or terminates the running job's complete process tree.
- Immediate shutdown cancels queued and running work before worker join and persists each cancelled record as rollback-readable `interrupted` state.
- Unix jobs run in isolated process groups with TERM/KILL escalation.
- GNU/Linux delimits negative process-group IDs with `--` and distinguishes terminated zombies from live descendants through `/proc` state inspection.
- Windows jobs run in isolated process groups and terminate through `taskkill /T /F`.
- Cancellation failure remains visible with platform error context; Medusa never silently claims descendant termination succeeded.

Cross-platform acceptance evidence includes:

- eight simultaneous frontend supervisors launching exactly one daemon
- restart after disconnection
- 64 simultaneous reconnecting clients
- exact one-worker/one-queue backpressure
- persisted graceful draining
- queued cancellation that never executes
- running descendant-tree termination within a bounded interval
- unrelated-process isolation
- bounded immediate shutdown and restart-readable state

See [Daemon operations](crates/medusa-daemon/README.md) and [Daemon concurrency and backpressure](docs/DAEMON-CONCURRENCY.md).

## Browser tools

The agent exposes `browser_navigate`, `browser_snapshot`, `browser_click`, `browser_fill`, `browser_press`, `browser_screenshot`, `browser_evaluate`, `browser_tabs`, and `browser_close`.

The browser runs in a separate `medusa-browserd` process. Medusa discovers it next to the agent binary or on `PATH`; set `MEDUSA_BROWSER_PATH` to override. Node.js 22 and Chromium are required.

## Desktop Commander MCP

Desktop Commander is optional and disabled by default:

```bash
export MEDUSA_DESKTOP_COMMANDER_ENABLED=true
medusa doctor
medusa
```

Medusa launches pinned `@wonderwhy-er/desktop-commander@0.2.46` through `npx`, uses an isolated home under `.medusa/extensions/desktop-commander`, disables telemetry and onboarding, clears inherited credentials, and limits allowed directories to the active repository.

The default capability set is read-only. Enable write tools explicitly:

```bash
export MEDUSA_DESKTOP_COMMANDER_ALLOW_WRITE=true
```

Desktop Commander process and terminal tools are not exposed. Use Medusa's native `shell_run`, which remains subject to command policy and sandbox controls.

## Headless commands

```bash
medusa run "Fix the failing tests"
medusa resume <session-id>
medusa doctor
medusa migrate
medusa search <pattern>
medusa shell <program> [args...]
medusa checkpoint "message"
```

## Configuration

Project configuration is loaded from `.medusa/config.toml`. Provider credentials are read from environment variables and are not written into repository state.

Run `medusa doctor` to validate tools, repository access, writable state, schema compatibility, provider credentials, the configured model, and enabled integrations.

### Coding policy

`MEDUSA_CODING_POLICY` controls the always-on implementation policy for non-read-only model turns. It defaults to `full`; valid values are `off`, `lite`, `full`, and `ultra`. Invalid or unset values fall back to `full`.

## Safety model

Medusa is autonomous, but not boundary-free. The runtime enforces:

- repository-relative filesystem containment and symlink checks
- atomic writes and guarded multi-file transactions
- hard denial of destructive shell and Git operations
- isolated worker worktrees and deterministic conflict handling
- environment and credential redaction
- checksummed sessions, extensions, and operational evidence
- reversible migrations and rollback receipts
- explicit verification evidence before completion

## Architecture

| Crate | Responsibility |
|---|---|
| `medusa-cli` | User-facing command entry point |
| `medusa-runtime` | Frontend-neutral interactive session control, commands, events, cancellation, follow-ups, and manager composition |
| `medusa-tui` | Terminal presentation, composer, clipboard, drafts, rendering, and daemon lifecycle visibility |
| `medusa-daemon` | Cross-platform IPC, shared lifecycle supervision, bounded scheduling, overload backpressure, race-safe cancellation, descendant-safe immediate shutdown, persistence, recovery, and graceful draining |
| `medusa-agent` | Agent Orchestrator: session lifecycle, planning, minimal coding policy injection, completion verification, and the shared Tool Manager |
| `medusa-capabilities` | Capability Manager: one discovered capability matrix for CLI, TUI, desktop, and model context |
| `medusa-provider` | Provider Manager: provider-neutral contracts, bounded retry/failover, response cache, and health snapshots |
| `medusa-github` | GitHub Manager: authenticated repository, pull request, issue, and Actions operations via GitHub CLI credential storage |
| `medusa-update` | Update Manager: main-branch discovery, verified-release primitives, platform installation, rollback, and restart |
| `medusa-intelligence` | Parsing, indexing, patching, and conflict-aware transactions |
| `medusa-memory` | Markdown storage, retrieval, provenance, and lifecycle |
| `medusa-workers` | Parallel worktrees and deterministic merge coordination |
| `medusa-extensions` | Skills, hooks, MCP isolation, and Desktop Commander integration |
| `medusa-hardening` | Observability, migrations, archives, chaos recovery, and release evidence |
| `medusa-browser-client` | Browser sidecar client and protocol |
| `medusa-browserd` | Node.js and Playwright browser sidecar process |

The manager boundaries are deliberately one-way: frontends depend on `medusa-runtime`; runtime composes capability, provider, and agent managers; the agent consumes the Tool Manager and capability context; service managers stay independent of presentation. This keeps future GitLab, Bitbucket, Azure DevOps, package sources, MCP servers, and model providers additive rather than changes to a monolithic runtime.

## Desktop interface

`apps/medusa-desktop` is the Zeus-derived alternative entry point. It preserves the three-panel desktop shell while replacing Zeus's separate agent implementation with a thin Tauri adapter over `medusa-runtime`.

```bash
cd apps/medusa-desktop
npm ci
npm run tauri:dev
```

Build the validated unsigned package targets for the current platform with `npm run tauri:build -- --bundles <targets>`. Linux uses `deb,appimage`, macOS uses `app,dmg`, and Windows uses `nsis`. See [Desktop distribution](docs/DESKTOP-DISTRIBUTION.md) for package validation, CI artifacts, draft-release assembly, provenance verification, and signing limitations.

The desktop app uses the same session controller, provider configuration, skills, cancellation, follow-up queue, plans, questions, tools, memory, policy, and repository-scoped daemon supervisor as the TUI.

## Development and verification

Use the same checks enforced by CI:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo clippy --workspace --all-features --locked --lib --bins --examples -- -D clippy::unwrap_used -D clippy::expect_used -D clippy::panic
cargo test --workspace --all-features --locked
cargo clippy -p medusa-daemon -p medusa-tui --all-targets --locked -- -D warnings
cargo test -p medusa-daemon -p medusa-tui --locked -- --nocapture
python3 scripts/dependency-metrics.py measure --root . --output dependency-current.json
python3 scripts/check-desktop-version-sync.py --root . --self-test
python3 scripts/desktop-package-smoke.py --self-test
python3 scripts/release-evidence.py self-test
python3 scripts/release-evidence.py sbom --root . --output release-sbom.json
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --all-features --locked --no-deps
cargo deny check advisories sources
cargo audit
bash scripts/check-source-size.sh
bash scripts/check-workflow-hygiene.sh
```

Release Gates additionally run workspace coverage with a 75% threshold, named adversarial regressions, fuzz and chaos checks, cross-platform release packages, documentation/schema validation, security gates, and three live MiniMax autonomous coding scenarios. The Desktop workflow separately builds and smoke-validates unsigned application bundles on Linux, macOS, and Windows whenever desktop or release-packaging logic changes. The tag-only publication workflow remains inert until a version tag is pushed.

## Documentation

- [Contributing](CONTRIBUTING.md)
- [Release process](docs/RELEASE.md)
- [Release compatibility](docs/COMPATIBILITY.md)
- [Desktop distribution](docs/DESKTOP-DISTRIBUTION.md)
- [Observability](docs/OBSERVABILITY.md)
- [Security hardening](docs/SECURITY-HARDENING.md)
- [Capability evidence](docs/CAPABILITY-EVIDENCE.md)
- [Daemon operations](crates/medusa-daemon/README.md)
- [Daemon concurrency and backpressure](docs/DAEMON-CONCURRENCY.md)
- [Dependency hygiene evidence](docs/DEPENDENCY-HYGIENE.md)

## License

Medusa is licensed under the [MIT License](LICENSE).

## Provider configuration

On first interactive launch, Medusa creates a non-secret provider profile in the platform user configuration directory. The profile controls the real runtime used by the TUI and headless commands.

Supported protocols:

- Anthropic Messages API: MiniMax, Anthropic, and compatible endpoints
- OpenAI Chat Completions API: OpenAI-compatible gateways, OmniRoute, Ollama-compatible servers, and local endpoints

Credentials are never written to `provider.toml`. Use a provider-specific `<PROVIDER>_API_KEY`, `OPENAI_API_KEY`, `MEDUSA_API_KEY`, or the selected gateway's existing authentication. `medusa config show` displays only non-secret settings and `medusa config reset` removes the profile.