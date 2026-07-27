<p align="center">
  <img src="assets/medusa-banner.png" alt="Medusa — The Self-Improving Coding Agent" width="100%">
</p>

# Medusa

Medusa is a repository-aware coding agent written in Rust. It can inspect an existing codebase, plan a change, edit files, run guarded commands, verify the result, preserve the session, and continue later through a terminal interface, desktop application, or headless CLI command.

Medusa is strongest when the task belongs inside a real repository: fixing a failing test, tracing a bug across files, making a bounded feature change, reviewing impact, running the relevant checks, and leaving durable evidence of what happened.

## What Medusa is good at

- **Working in existing repositories.** It searches and reads the project before editing, uses repository-aware file and Git tools, and keeps mutations inside guarded transaction boundaries.
- **Small, verified changes.** The runtime prefers the smallest complete fix, records changed paths, selects targeted checks when possible, and requires verification before an autonomous coding session is considered complete.
- **Long-running work.** Sessions, plans, prompts, tool activity, verification evidence, and recovery state are stored under the repository's `.medusa` directory and can be resumed.
- **Showing its work.** The TUI and desktop app share the same runtime event stream, including assistant output, plans, tool activity, questions, approvals, failures, and completion state.
- **Operating with explicit boundaries.** Repository writes are path-checked and transactional, sensitive external paths remain denied, dangerous shell operations fail closed, and approval grants are tied to the exact action and current plan.
- **Learning from outcomes.** Verified sessions can contribute bounded repository-local recall and Markdown lessons. Failed sessions retain failure history and negative skill outcomes instead of being counted as successful experience.

Medusa is not a general-purpose shell replacement. It intentionally rejects commands and paths that would weaken its containment or persistence boundaries.

## Interfaces

Medusa has one production runtime with three user-facing entry points:

| Interface | Use it for |
|---|---|
| Terminal UI | Interactive coding sessions, questions, approvals, attachments, plans, and live activity. |
| Desktop app | The same runtime in a React/Tauri interface with a central execution timeline, session navigation, diffs, memory views, settings, and approvals. |
| Headless CLI | Scripted objectives, resumable sessions, and unattended runs with an explicit command-approval allowlist. |

### Terminal UI

<p align="center">
  <img src="docs/assets/medusa-tui.png" alt="Medusa terminal interface during a verified coding task" width="100%">
</p>

### Desktop application

<p align="center">
  <img src="docs/assets/medusa-desktop.png" alt="Medusa desktop application showing chat, plan, activity, and usage" width="100%">
</p>

## Installation

### Prerequisites

- Git
- Rust 1.88 or newer; this repository pins Rust 1.88.0
- A supported model connection
- Node.js 22 for ChatGPT OAuth, browser verification, desktop development, or desktop packaging

### Install the CLI from `main`

```bash
cargo install --git https://github.com/benclawbot/Medusa.git --locked medusa-cli
```

Confirm the installation:

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

The release workflow builds unsigned desktop packages for Linux, macOS, and Windows:

- Linux: Debian package and AppImage
- macOS: application archive and DMG
- Windows: NSIS installer

Release assets remain draft-only until a maintainer reviews the packages, checksums, SBOM, and provenance. Windows packages are not Authenticode-signed, macOS packages are not Developer ID signed or notarized, and Linux packages are not distributed through a signed package repository.

For desktop development from source, install Node.js 22 and use the scripts in `apps/medusa-desktop`.

## First run

Run Medusa inside a repository:

```bash
cd /path/to/project
medusa
```

On the first interactive launch, Medusa asks you to choose a model connection and stores the non-secret profile in your user configuration directory:

- Linux and macOS: `${XDG_CONFIG_HOME:-~/.config}/medusa/provider.toml`
- Windows: `%APPDATA%\medusa\provider.toml`

API keys are read from the environment and are not written to `provider.toml`.

### Direct provider routes

The Rust provider layer directly implements Anthropic Messages-compatible routes:

| Route | Credential |
|---|---|
| MiniMax | `MINIMAX_API_KEY` |
| Anthropic | `ANTHROPIC_API_KEY` |
| Anthropic-compatible endpoint | `MEDUSA_API_KEY` and optionally `MEDUSA_BASE_URL` |

MiniMax is the default route. Review or reset the saved connection with:

```bash
medusa config show
medusa config reset
medusa config
```

## Everyday use

Open the interactive terminal in the current repository:

```bash
medusa
```

Open another repository or begin with a prompt:

```bash
medusa --repo /path/to/project
medusa --prompt "Fix the failing tests and verify the result"
```

Resume a known session or continue the most recent one:

```bash
medusa --resume <session-id>
medusa --continue
```

Run a headless objective:

```bash
medusa run "Fix the failing tests and verify the result"
```

A normal headless run stops if the agent needs user input. For unattended approval of known shell commands, create an allowlist containing one exact command per line:

```text
# .medusa/approve.txt
cargo test --workspace
cargo fmt --all -- --check
```

Then run:

```bash
medusa run \
  --non-interactive \
  --approve-allowlist .medusa/approve.txt \
  "Fix the failing tests and verify the result"
```

The allowlist does not bypass policy. The runtime still validates the exact action, current plan fingerprint, command restrictions, containment, and approval expiry before execution.

Useful maintenance commands:

```bash
medusa doctor
medusa migrate
medusa update --check
medusa update
```

`medusa update --check` is read-only. Source-installed binaries can update from a verified immutable commit on `main`; package-managed installations are not overwritten and instead report the relevant package-manager command.

## Configuration

Medusa loads typed configuration with unknown fields denied. The principal runtime settings cover agent mode and turn limits, provider route and model details, retry policy, Markdown memory, and required verification.

Command-line overrides use `--set key=value`:

```bash
medusa --set agent.mode=read-only
medusa --set verification.browser_on_ui_change=false
```

Fallback providers are complete routes with their own provider, model, protocol, endpoint, authentication mode, capabilities, and retry policy. A fallback does not inherit the primary provider's credentials or request-specific fields.

See [Configuration](docs/CONFIGURATION.md) for the complete supported schema.

## Verification and safety

Medusa treats successful model output and successful repository work as different things. Coding completion requires repository verification.

After mutations, the runtime can build a code index, inspect changed symbols and affected files, select impacted tests, detect public API risk, and run targeted commands. It falls back to broader repository checks when semantic selection is unavailable or unsafe.

Shell execution fails closed when the platform containment backend is unavailable:

- Linux uses Bubblewrap.
- macOS uses Seatbelt.
- Windows uses the Windows 11 composable sandbox API with repository read/write binding, toolchain read-only binding, network denial, an environment allowlist, and Job Object limits.

Windows command containment requires Windows 11 with `Experimental_CreateProcessInSandbox` available. There is no unsandboxed fallback through the sandbox API.

Repository writes are symlink-aware and transactional. `.git`, credential locations, operating-system configuration and executable paths, and login-persistence locations remain denied even when an external write receives approval.

## Persistent state and learning

Repository-local state is stored under `.medusa`, including sessions, prompts, daemon records, verification evidence, memory, failure history, skill outcomes, checkpoints, and transaction journals.

Medusa's durable memory is Markdown-first. Recall is repository-scoped and bounded; it does not treat every previous session as trustworthy. Completed-session learning requires verified evidence, starts accepted lessons in probation, and preserves provenance.

## Built-in integrations

### ChatGPT OAuth and OpenAI-compatible gateways

First-run setup supports local or externally supplied gateways, including OmniRoute, OpenAI-compatible endpoints, local model runtimes, the OpenAI API, and ChatGPT OAuth.

ChatGPT OAuth is provided through the separately distributed `openai-oauth` loopback gateway. Medusa expects it at `127.0.0.1:10531` and can start it with:

```bash
npx --yes openai-oauth@latest --detach
```

The gateway owns the OAuth credential. Medusa communicates with its local OpenAI-compatible endpoint and does not read the gateway credential file.

### Browser verification

When `verification.browser_on_ui_change` is enabled, Medusa automatically verifies effective UI changes through the Playwright sidecar. Documentation-only, generated, snapshot-only, lockfile, and build-output changes are excluded.

A browser run records the route, assertions, screenshots, console errors, override state, and result. Node.js, the browser sidecar, and a reachable development route are required. Missing prerequisites produce an actionable failure rather than a false pass.

### Image prompts

The runtime supports image message blocks. Screenshots are sent only when the selected provider declares compatible image-input support, media types, byte limits, and image-count limits.

### Desktop Commander

Desktop Commander is isolated behind its extension boundary. The normal repository tools, runtime, TUI, and desktop application do not depend on it.

## Platform support

The canonical workflows test the Rust workspace and daemon behavior across Linux, macOS, and Windows. Desktop CI builds and validates unsigned packages on all three platforms. Release gates also cover coverage, security checks, adversarial regressions, fuzz smoke tests, chaos and migration recovery, documentation/schema consistency, package smoke tests, and live provider scenarios.

Platform support does not imply identical containment internals or operating-system signing.

## Current limitations

- ChatGPT OAuth depends on the separately distributed `openai-oauth` gateway and Node.js.
- Browser verification depends on Node.js, Playwright-sidecar availability, and a reachable development route.
- Provider streaming is represented in configuration and capability checks, but the native Anthropic-compatible adapter currently performs non-streaming requests.
- Screenshot input is accepted only when the selected provider declares compatible image support.
- Desktop release packages are unsigned at the operating-system level.
- Windows command containment requires the Windows 11 composable sandbox API.

## Project documentation

The README is the product overview and installation guide. Retained documentation describes current operation:

- [Configuration](docs/CONFIGURATION.md)
- [Release process](docs/RELEASE.md)
- [Release compatibility](docs/COMPATIBILITY.md)
- [Security hardening](docs/SECURITY-HARDENING.md)
- [Observability](docs/OBSERVABILITY.md)
- [Desktop distribution](docs/DESKTOP-DISTRIBUTION.md)
- [Capability evidence](docs/CAPABILITY-EVIDENCE.md)

## Development

Use the pinned toolchain and run the repository gates relevant to your change:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo doc --workspace --all-features --no-deps
```

Frontend and desktop changes must also pass the checks defined in `apps/medusa-desktop` and the Desktop workflow.

Medusa is licensed under the MIT License.
