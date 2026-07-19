<p align="center">
  <img src="assets/medusa-banner.png" alt="Medusa — The Self-Improving Coding Agent" width="100%">
</p>

# Medusa

Medusa is a production-grade autonomous coding agent written in Rust. It combines an interactive terminal experience, a shared desktop runtime, repository-aware tools, durable Markdown memory, guarded execution, persistent background jobs, multi-worker coordination, browser verification, and release-grade validation.

## Highlights

- **Interactive by default** — run `medusa` inside a repository to open the terminal interface.
- **Autonomous coding loop** — inspect, plan, edit, verify, and iterate until completion evidence is available.
- **Shared frontend runtime** — the terminal and Zeus-derived React/Tauri desktop entry points use the same `medusa-runtime` session controller instead of separate agent stacks.
- **Durable sessions and drafts** — resume work after interruption without losing prompt or execution state.
- **Mid-turn guidance** — submit extra detail while Medusa is working; the user turn remains visible and is injected at the next safe agent-turn boundary.
- **Clipboard-native input** — paste text or screenshots with `Ctrl+V`; supported providers receive screenshots as image context.
- **Repository-aware tooling** — bounded file access, search, atomic writes, patch transactions, shell execution, Git checkpoints, and targeted verification.
- **Persistent background jobs** — repository-scoped daemon jobs, bounded workers and queues, overload backpressure, ownership, reconnect, durable state, and restart recovery work on Linux, macOS, and Windows.
- **Browser and web interaction** — a persistent Playwright sidecar can navigate, click, fill, press, capture screenshots, evaluate JavaScript, and manage tabs.
- **Markdown conversation display** — headings, lists, task boxes, quotes, links, rules, and fenced code blocks render directly in the terminal.
- **Persistent memory** — Markdown-first storage with validation, indexing, retrieval, lifecycle management, and provenance controls.
- **Parallel workers** — isolated worktrees, deterministic merge behavior, conflict detection, and cleanup safeguards.
- **Extensions and MCP** — skills, hooks, MCP isolation, the optional Desktop Commander adapter, redaction, and checksummed provenance.
- **Production hardening** — panic-free production targets, least-privilege workflow guards, bounded-load evidence, dependency graph metrics, migrations, rollback bundles, archive safety, fuzzing, chaos recovery, package smoke tests, and live-provider validation.

## Current status and evidence

The original phase labels are historical planning shorthand, not the current source of truth. As of July 19, 2026, repository evidence through PR #52 includes the Rust agent core, interactive TUI, frontend-neutral runtime, Zeus-derived React/Tauri desktop entry point, durable sessions and memory, guarded repository tools, browser verification, parallel workers, Markdown rendering, mid-turn follow-ups, optional Desktop Commander MCP integration, panic-free production targets, workflow-write guardrails, cross-platform daemon transport and recovery, TUI connection visibility, bounded daemon workers and queues, explicit overload backpressure, graceful draining, cross-platform load evidence, and evidence-based dependency pruning with permanent base/current graph metrics.

| Area | Current evidence |
|---|---|
| Interactive product surface | `medusa` launches the TUI; transcript preservation, Markdown rendering, clipboard input, cancellation, usage metrics, skills, queued follow-ups, and daemon connection transitions are implemented in `medusa-tui`. |
| Agent and repository runtime | `medusa-runtime` owns frontend-neutral interactive session control, while planning, tools, policy, verification, intelligence, and persistence remain implemented across `medusa-agent`, `medusa-intelligence`, `medusa-memory`, and related crates. |
| Background daemon | `medusa-daemon` provides one protocol and durable lifecycle across Linux, macOS, and Windows. It uses four fixed job workers and a 32-job queue by default, returns `daemon_busy` under overload, bounds local I/O and request size, and drains accepted work during graceful shutdown. Reconnect, ownership, recovery, a 64-client burst, exact queue backpressure, and shutdown persistence are tested on all three platforms. |
| Shared frontend runtime and desktop | `medusa-tui` and `apps/medusa-desktop` adapt the same `medusa-runtime` commands, events, plans, questions, cancellation, follow-ups, skills, provider settings, and policy. |
| Extensions and MCP | Skills, hooks, MCP isolation, and the pinned Desktop Commander adapter are implemented in `medusa-extensions`. |
| Dependency hygiene | Locked Cargo metadata is compared against the PR base in the read-only dependency-policy job. PR #52 removed five proven-unused direct edges while keeping locked/resolved packages at 297, duplicate counts unchanged, and enabled feature selections unchanged; no unsupported build-speed claim is made. |
| Release evidence | `CI`, `Daemon`, `Desktop`, `Refactor Guardrails`, and `Release Gates` enforce formatting, Clippy, panic-free production targets, workspace tests, documentation, source-size limits, workflow hygiene, dependency base/current metrics, cargo-deny, cargo-audit, three-platform daemon/TUI and desktop checks, bounded-load evidence, coverage, adversarial tests, package smoke tests, and live-provider scenarios. |

See [Capability evidence](docs/CAPABILITY-EVIDENCE.md) for the auditable mapping from shipped capabilities to code and gates. Historical completion summaries should not override the current repository, merged pull requests, or required checks.

## Requirements

- Git
- Rust 1.88 or newer and Cargo; the repository pins Rust 1.88.0
- `MINIMAX_API_KEY` for live MiniMax execution
- Node.js 22 only when browser verification or Desktop Commander is enabled

## Installation

Medusa is currently installed from source. The first optimized build can take several minutes because Cargo compiles Medusa and its dependencies locally.

### Fast path

```bash
git clone https://github.com/benclawbot/Medusa.git
cd Medusa
cargo install --path crates/medusa-cli --locked
medusa --version
medusa doctor
```

`medusa doctor` reports a failure until `MINIMAX_API_KEY` is configured. The remaining diagnostics still verify Git, Cargo, repository access, writable state, schema support, and optional integrations.

### Windows

Install Git and Rustup with Winget. Install Node.js 22 as well when browser verification or Desktop Commander is needed.

```powershell
winget install --id Git.Git -e --accept-package-agreements --accept-source-agreements
winget install --id Rustlang.Rustup -e --accept-package-agreements --accept-source-agreements
winget install --id OpenJS.NodeJS.22 -e --accept-package-agreements --accept-source-agreements
```

Open a new PowerShell window, then install the pinned toolchain:

```powershell
rustup toolchain install 1.88.0 --profile minimal --component clippy,rustfmt
rustup default 1.88.0
```

Ensure Cargo-installed programs are available:

```powershell
$cargoBin = Join-Path $HOME '.cargo\bin'
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$entries = @($userPath -split ';' | Where-Object { $_ })
if ($entries.TrimEnd('\') -notcontains $cargoBin.TrimEnd('\')) {
    [Environment]::SetEnvironmentVariable('Path', (($entries + $cargoBin) -join ';'), 'User')
}
$env:Path = "$cargoBin;$env:Path"
```

Clone, install, configure, and verify:

```powershell
git clone https://github.com/benclawbot/Medusa.git
Set-Location Medusa
cargo install --path crates/medusa-cli --locked
$env:MINIMAX_API_KEY = '<your-key>'
medusa --version
medusa doctor
```

Do not put a real API key in the repository. Use a credential or environment-management tool for persistent storage.

### macOS

Install Apple's command-line developer tools:

```bash
xcode-select --install
```

Install Git and optional Node.js 22 with your package manager, then install Rustup and the pinned toolchain:

```bash
source "$HOME/.cargo/env"
rustup toolchain install 1.88.0 --profile minimal --component clippy,rustfmt
rustup default 1.88.0
```

Clone, install, configure, and verify:

```bash
git clone https://github.com/benclawbot/Medusa.git
cd Medusa
cargo install --path crates/medusa-cli --locked
export MINIMAX_API_KEY='<your-key>'
medusa --version
medusa doctor
```

### Linux

Install Git, a C/C++ build toolchain, `curl`, and `pkg-config` using the distribution package manager. Debian or Ubuntu:

```bash
sudo apt update
sudo apt install -y build-essential git curl pkg-config
```

Install Rustup and the pinned toolchain:

```bash
source "$HOME/.cargo/env"
rustup toolchain install 1.88.0 --profile minimal --component clippy,rustfmt
rustup default 1.88.0
```

Clone, install, configure, and verify:

```bash
git clone https://github.com/benclawbot/Medusa.git
cd Medusa
cargo install --path crates/medusa-cli --locked
export MINIMAX_API_KEY='<your-key>'
medusa --version
medusa doctor
```

### Installation troubleshooting

When `cargo` or `medusa` is not found, open a new terminal first. On macOS or Linux, run `source "$HOME/.cargo/env"`. On Windows, confirm `%USERPROFILE%\.cargo\bin` is in the user `PATH`.

When Rustup reports a partial or conflicting toolchain installation:

```bash
rustup toolchain uninstall 1.88.0
rustup toolchain install 1.88.0 --profile minimal --component clippy,rustfmt
rustup default 1.88.0
```

## Quick start

Open the interactive terminal in the current repository:

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
| `Enter` | Submit the current prompt, or queue a follow-up while Medusa is working |
| `Shift+Enter` | Insert a new line |
| `Ctrl+V` | Paste clipboard text or attach a screenshot |
| `Ctrl+C` | Cancel the active task; press twice within one second to exit |
| `Ctrl+D` | Exit when the composer is empty |
| `Esc` | Cancel the active task or close the current modal |

Prompt drafts and clipboard attachments persist under the repository's `.medusa` directory until submission. If the runtime rejects a submission, the draft is restored. Mid-turn follow-ups remain visible immediately and are applied before the next model turn.

Installed skills are directly invokable by name. Built-in commands take precedence over same-named skills:

```text
/skills                         # list installed skills
/release                        # select the release skill for the next prompt
/release prepare version 1.0    # run a task with the release skill immediately
/release@user                   # select a scoped definition when names collide
```

Selected skill instructions are ephemeral system context and are not written into durable session messages. Typing `/` filters built-in commands and installed skills; Tab completes the selected entry.

The TUI header reports session duration, cumulative input/output tokens, cache-read tokens and hit percentage, and output throughput. `/new` resets session metrics.

## Background daemon

`medusa-daemon` owns repository-scoped background jobs, reconnectable local IPC, durable job records, process ownership, bounded execution, restart recovery, and graceful shutdown.

- Linux and macOS use `.medusa/daemon/medusa.sock` as a Unix-domain socket.
- Windows uses the same path as an endpoint descriptor containing an ephemeral loopback TCP address; non-loopback descriptors are rejected.
- A new connection is used for each request, so clients can disconnect while daemon-owned jobs continue.
- Production defaults are four concurrent job workers and 32 queued jobs; no new operating-system thread is created per submission.
- When the queue is full, the submission receives `daemon_busy` and no durable job record is retained for rejected work.
- Local reads and writes time out after five seconds, and requests larger than 64 KiB are rejected.
- Graceful shutdown stops new request acceptance, drains queued and running accepted jobs, joins workers, and then releases endpoint ownership.
- Queued or running jobs found after an ungraceful restart are marked `interrupted` with recovery evidence.
- Stale ownership is reclaimed only when the recorded process is no longer alive.
- The TUI reports daemon connection-state transitions on Linux, macOS, and Windows without flooding the transcript.

The permanent `Daemon` workflow validates the daemon/TUI contract on Ubuntu, macOS, and Windows. Its acceptance evidence includes 64 simultaneous reconnecting clients, exact one-worker/one-queue backpressure, and persisted graceful draining. See [the daemon operations guide](crates/medusa-daemon/README.md) and [the concurrency decision](docs/DAEMON-CONCURRENCY.md).

**Current limitation:** one shared external lifecycle owner for TUI and desktop has not yet been selected. Automatic executable discovery, startup race handling, restart policy, process-tree cancellation, coordinated shutdown, and visible degraded/recovery states remain issue #42 work. Graceful shutdown currently waits for running children to finish, so a hung child process tree can delay completion. The TUI observes an available daemon; it does not silently create an in-process substitute that would die with the frontend.

## Browser tools

The agent can drive a headless browser with `browser_navigate`, `browser_snapshot`, `browser_click`, `browser_fill`, `browser_press`, `browser_screenshot`, `browser_evaluate`, `browser_tabs`, and `browser_close`.

The browser runs in a separate `medusa-browserd` process. Medusa discovers it next to the agent binary or on `PATH`; set `MEDUSA_BROWSER_PATH` to override. The sidecar requires Node.js 22 and Chromium.

## Desktop Commander MCP

Desktop Commander is optional and disabled by default. Medusa launches a pinned `@wonderwhy-er/desktop-commander@0.2.46` stdio server through `npx`, performs the MCP initialize/list/call lifecycle, and keeps it alive for the agent session.

```bash
export MEDUSA_DESKTOP_COMMANDER_ENABLED=true
medusa doctor
medusa
```

The integration uses an isolated Medusa-owned home under `.medusa/extensions/desktop-commander`, disables telemetry and onboarding, clears inherited credentials, and limits allowed directories to the active repository. Medusa independently validates path arguments, rejects traversal and symlink escapes, caps and redacts output, and treats returned content as untrusted.

The default capability set is read-only. Enable write tools explicitly:

```bash
export MEDUSA_DESKTOP_COMMANDER_ALLOW_WRITE=true
```

Desktop Commander process and terminal tools are not exposed. Use Medusa's native `shell_run`, which remains subject to command policy and sandbox controls.

Advanced overrides:

```bash
export MEDUSA_DESKTOP_COMMANDER_ALLOWED_TOOLS='read_file,list_directory,start_search,get_more_search_results,write_file'
export MEDUSA_DESKTOP_COMMANDER_COMMAND='npx'
export MEDUSA_DESKTOP_COMMANDER_ARGS='["-y","@wonderwhy-er/desktop-commander@0.2.46","--no-onboarding"]'
export MEDUSA_DESKTOP_COMMANDER_TIMEOUT_MS=30000
export MEDUSA_DESKTOP_COMMANDER_MAX_OUTPUT_BYTES=262144
```

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

Project configuration is loaded from:

```text
.medusa/config.toml
```

Provider credentials are read from environment variables and are not written to repository state:

```bash
export MINIMAX_API_KEY="..."
```

Run `medusa doctor` to validate tools, repository access, writable state, schema compatibility, provider credentials, the configured model, and enabled integrations.

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

See [Security hardening](docs/SECURITY-HARDENING.md) for release-enforced controls.

## Architecture

| Crate | Responsibility |
|---|---|
| `medusa-cli` | User-facing command entry point |
| `medusa-runtime` | Frontend-neutral interactive session controller, commands, events, cancellation, follow-ups, and provider orchestration |
| `medusa-tui` | Terminal presentation, composer, clipboard, drafts, rendering, and daemon connection-state observation |
| `medusa-daemon` | Cross-platform local IPC, bounded background-job scheduling, overload backpressure, ownership, reconnect, persistence, restart recovery, and graceful draining |
| `medusa-agent` | Session lifecycle, orchestration, tools, policy, and verification |
| `medusa-provider` | Provider-neutral model interface and MiniMax integration |
| `medusa-intelligence` | Parsing, indexing, patching, and conflict-aware transactions |
| `medusa-memory` | Markdown storage, retrieval, provenance, and lifecycle |
| `medusa-workers` | Parallel worktrees and deterministic merge coordination |
| `medusa-extensions` | Skills, hooks, and MCP execution |
| `medusa-hardening` | Observability, migrations, archives, chaos recovery, and release evidence |
| `medusa-browser-client` | Browser sidecar client and protocol |
| `medusa-browserd` | Node.js and Playwright browser sidecar process |

## Desktop interface

`apps/medusa-desktop` is the Zeus-derived alternative entry point. It preserves the three-panel desktop shell and interaction model while replacing Zeus's separate agent implementation with a thin Tauri adapter over `medusa-runtime`.

```bash
cd apps/medusa-desktop
npm install
npm run tauri:dev
```

The desktop app opens a repository explicitly and uses the same session controller, provider configuration, skills, cancellation, follow-up queue, plans, questions, tools, memory, and policy as the terminal entry point. Attachments are confined to the selected repository; pasted images are decoded and validated by the Rust adapter before entering the runtime.

Desktop daemon lifecycle ownership is not yet wired. That work will use the same `medusa-daemon` contract rather than introduce another backend.

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
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --all-features --locked --no-deps
cargo deny check advisories sources
cargo audit
bash scripts/check-source-size.sh
bash scripts/check-workflow-hygiene.sh
```

Release Gates additionally run complete workspace coverage with a 75% line threshold, named adversarial regressions, fuzz and chaos smoke tests, cross-platform release-package smoke tests, and three live MiniMax autonomous coding scenarios.

## Documentation

- [Contributing](CONTRIBUTING.md)
- [Release process](docs/RELEASE.md)
- [Observability](docs/OBSERVABILITY.md)
- [Security hardening](docs/SECURITY-HARDENING.md)
- [Capability evidence](docs/CAPABILITY-EVIDENCE.md)
- [Daemon operations](crates/medusa-daemon/README.md)
- [Daemon concurrency and backpressure](docs/DAEMON-CONCURRENCY.md)
- [Dependency hygiene evidence](docs/DEPENDENCY-HYGIENE.md)

## License

Medusa is licensed under the [MIT License](LICENSE).
