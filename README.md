<p align="center">
  <img src="assets/medusa-banner.png" alt="Medusa — The Self-Improving Coding Agent" width="100%">
</p>

# Medusa

Medusa is a production-grade autonomous coding agent written in Rust. It combines an interactive terminal experience with a persistent agent runtime, repository-aware tools, durable Markdown memory, guarded execution, multi-worker coordination, browser verification, and release-grade validation.

## Highlights

- **Interactive by default** — run `medusa` inside a repository to open the terminal interface.
- **Autonomous coding loop** — inspect, plan, edit, verify, and iterate until completion evidence is available.
- **Durable sessions and drafts** — resume work after interruption without losing prompt or execution state.
- **Clipboard-native input** — paste text or screenshots with `Ctrl+V`; screenshots are encoded and submitted as image context when the configured provider supports it.
- **Repository-aware tooling** — bounded file access, search, atomic writes, patch transactions, shell execution, Git checkpoints, and targeted verification.
- **Browser and web interaction** — a persistent headless browser the agent can drive from tool calls (navigate, click, fill, press, screenshot, evaluate JS, list tabs).
- **Full-content display** — every tool result, fetched page, and shell run is shown in full in the TUI; long content is paged with `Shift+Up` / `Shift+PgUp` instead of being truncated.
- **Persistent memory** — Markdown-first memory with validation, indexing, retrieval, lifecycle management, and provenance controls.
- **Parallel workers** — isolated worktrees, deterministic merge behavior, conflict detection, and cleanup safeguards.
- **Extensions and browser evidence** — skills, hooks, MCP isolation, Playwright-based browser verification, output redaction, and checksummed provenance.
- **Production hardening** — migrations, rollback bundles, observability, archive safety, fuzzing, chaos recovery, dependency policy, package smoke tests, and live-provider validation.

## Requirements

- Git
- Rust 1.88 or newer and Cargo (the repository pins Rust 1.88.0)
- `MINIMAX_API_KEY` for live MiniMax execution
- Node.js 22 only when browser verification is enabled

## Installation

Medusa is currently installed from source. The first optimized build can take several minutes because Cargo compiles Medusa and its dependencies locally.

### Fast path

If Git, Rust 1.88 or newer, and Cargo are already available:

```bash
git clone https://github.com/benclawbot/Medusa.git
cd Medusa
cargo install --path crates/medusa-cli --locked
medusa --version
medusa doctor
```

`medusa doctor` exits with a failure until `MINIMAX_API_KEY` is configured. This is expected; the other checks still show whether Git, Cargo, repository access, state permissions, and schema support are ready.

### Windows (PowerShell)

Install Git and Rustup with Winget. Install Node.js 22 as well if you want browser verification:

```powershell
winget install --id Git.Git -e --accept-package-agreements --accept-source-agreements
winget install --id Rustlang.Rustup -e --accept-package-agreements --accept-source-agreements

# Optional: required only for browser verification
winget install --id OpenJS.NodeJS.22 -e --accept-package-agreements --accept-source-agreements
```

Close PowerShell and open a new window so the installers' environment changes are loaded. Then install Medusa's pinned Rust toolchain:

```powershell
rustup toolchain install 1.88.0 --profile minimal --component clippy,rustfmt
rustup default 1.88.0
```

Ensure Cargo-installed programs are available in the current session and future PowerShell windows:

```powershell
$cargoBin = Join-Path $HOME '.cargo\bin'
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$entries = @($userPath -split ';' | Where-Object { $_ })
if ($entries.TrimEnd('\') -notcontains $cargoBin.TrimEnd('\')) {
    $updatedPath = (($entries + $cargoBin) -join ';')
    [Environment]::SetEnvironmentVariable('Path', $updatedPath, 'User')
}
$env:Path = "$cargoBin;$env:Path"
```

Clone, install, and verify Medusa:

```powershell
git clone https://github.com/benclawbot/Medusa.git
Set-Location Medusa
cargo install --path crates/medusa-cli --locked
medusa --version
```

Configure the API key for the current PowerShell session, then run the full diagnostic:

```powershell
$env:MINIMAX_API_KEY = '<your-key>'
medusa doctor
```

Do not put a real API key in the repository. For persistent use, store it with your preferred Windows credential or environment-management tool and inject it into the shell that launches Medusa.

### macOS

Install Apple's command-line developer tools:

```bash
xcode-select --install
```

If Homebrew is not installed, use the installation command published at [brew.sh](https://brew.sh/) after reviewing it. Then install Git and, optionally, Node.js 22:

```bash
brew install git

# Optional: required only for browser verification
brew install node@22
brew link --overwrite node@22
```

Install Rustup using the command published at [rustup.rs](https://rustup.rs/). Review remote installer commands before running them:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustup toolchain install 1.88.0 --profile minimal --component clippy,rustfmt
rustup default 1.88.0
```

Clone, install, configure, and verify Medusa:

```bash
git clone https://github.com/benclawbot/Medusa.git
cd Medusa
cargo install --path crates/medusa-cli --locked
export MINIMAX_API_KEY='<your-key>'
medusa --version
medusa doctor
```

The `export` applies only to the current shell. Use your preferred secret manager or shell environment tooling for persistent use; never commit the key.

### Linux

Install Git, a C/C++ build toolchain, and `curl` using your distribution's package manager.

Debian or Ubuntu:

```bash
sudo apt update
sudo apt install -y build-essential git curl pkg-config
```

Fedora or RHEL-family systems:

```bash
sudo dnf group install -y "Development Tools"
sudo dnf install -y git curl pkgconf-pkg-config
```

Arch Linux:

```bash
sudo pacman -Syu --needed base-devel git curl pkgconf
```

Install Rustup, load Cargo into the current shell, and install the pinned toolchain. Review remote installer commands before running them:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustup toolchain install 1.88.0 --profile minimal --component clippy,rustfmt
rustup default 1.88.0
```

Node.js 22 is optional and is needed only for browser verification. Install it using your distribution's supported Node.js 22 package or the instructions at [nodejs.org](https://nodejs.org/en/download/package-manager); package names and versions vary by distribution.

Clone, install, configure, and verify Medusa:

```bash
git clone https://github.com/benclawbot/Medusa.git
cd Medusa
cargo install --path crates/medusa-cli --locked
export MINIMAX_API_KEY='<your-key>'
medusa --version
medusa doctor
```

### Verify prerequisites

These commands should all print versions. Skip `node --version` if browser verification is not needed:

```bash
git --version
rustc --version
cargo --version
node --version
medusa --version
medusa doctor
```

### Installation troubleshooting

If `cargo` or `medusa` is reported as an unknown command, open a new terminal first. On macOS or Linux, run `source "$HOME/.cargo/env"`. On Windows, confirm that `%USERPROFILE%\.cargo\bin` appears in the user `PATH`; the PowerShell `PATH` block above adds it without replacing existing entries.

If Rustup reports a partially installed or conflicting `1.88.0` toolchain, close other Rust and Cargo processes and retry:

```bash
rustup toolchain install 1.88.0 --profile minimal --component clippy,rustfmt
```

If the same conflict remains, remove and download only the pinned toolchain again. The first command deletes the local `1.88.0` toolchain before the second command reinstalls it; it does not remove your source repositories:

```bash
rustup toolchain uninstall 1.88.0
rustup toolchain install 1.88.0 --profile minimal --component clippy,rustfmt
rustup default 1.88.0
```

### Local development

To build without installing the binary globally:

```bash
git clone https://github.com/benclawbot/Medusa.git
cd Medusa
cargo build --release --locked
./target/release/medusa doctor
```

## Quick start

Open the interactive terminal in the current repository:

```bash
medusa
```

Open another repository:

```bash
medusa --repo /path/to/repository
```

Start with a prepared prompt:

```bash
medusa --prompt "Fix the failing tests and verify the result"
```

Resume a session:

```bash
medusa --resume <session-id>
```

Continue the most recent repository session:

```bash
medusa --continue
```

### Interactive controls

| Key | Action |
|---|---|
| `Enter` | Submit the current prompt |
| `Shift+Enter` | Insert a new line |
| `Ctrl+V` | Paste clipboard text or attach a screenshot |
| `Ctrl+C` | Cancel the active agent task; press twice within 1 second to exit |
| `Ctrl+D` | Exit when the composer is empty |
| `Esc` | Cancel the active agent task or close the current modal |

Prompt drafts and clipboard attachments are persisted under the repository's `.medusa` state directory until submission.

Installed skills are directly invokable by name. Built-in commands take precedence over same-named skills:

```text
/skills                         # list installed skills
/release                        # use the release skill on the next prompt
/release prepare version 1.0    # run a task with the release skill immediately
/release@user                   # select a scoped definition when names collide
```

Selected skill instructions are applied as ephemeral system context for the active task and are not written into durable session messages.

### Browser tools

The agent can drive a headless browser via the `browser_*` tools (`browser_navigate`, `browser_snapshot`, `browser_click`, `browser_fill`, `browser_press`, `browser_screenshot`, `browser_evaluate`, `browser_tabs`, `browser_close`). The browser runs in a separate `medusa-browserd` sidecar process. Medusa auto-discovers it next to the agent binary or on `PATH`; set `MEDUSA_BROWSER_PATH` to override. The sidecar requires Node.js 22 and a Chromium install (the same prerequisites the verification flow uses).

## Headless commands

The interactive interface is the default, while automation-oriented commands remain available:

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

Medusa loads project configuration from:

```text
.medusa/config.toml
```

Provider credentials are read from environment variables and are not written to repository state:

```bash
export MINIMAX_API_KEY="..."
```

Run `medusa doctor` to validate tools, repository access, writable state, schema compatibility, provider credentials, and the configured model.

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

See [Security hardening](docs/SECURITY-HARDENING.md) for the release-enforced controls.

## Architecture

| Crate | Responsibility |
|---|---|
| `medusa-cli` | User-facing command entry point |
| `medusa-tui` | Interactive terminal interface, composer, clipboard, drafts, and runtime bridge |
| `medusa-agent` | Session lifecycle, orchestration, tools, policy, and verification |
| `medusa-provider` | Provider-neutral model interface and MiniMax integration |
| `medusa-intelligence` | Parsing, indexing, patching, formatting, and test-impact analysis |
| `medusa-memory` | Markdown memory, validation, indexing, retrieval, and lifecycle |
| `medusa-workers` | Parallel worktrees, merge coordination, and conflict handling |
| `medusa-extensions` | Skills, hooks, MCP, browser integration, and redaction |
| `medusa-hardening` | Migrations, observability, release validation, archives, and chaos recovery |
| `medusa-daemon` | Persistent local job runtime and transport |
| `medusa-protocol` | Versioned events and cross-component contracts |
| `medusa-config` | Typed layered configuration |
| `medusa-core` | Shared identifiers and structured errors |

## Quality and release gates

Every pull request is checked with:

- formatting, Clippy, workspace tests, and documentation
- a **75% minimum workspace line-coverage gate**
- named adversarial security regressions
- dependency policy and vulnerability audit
- fuzz, migration, chaos, and rollback scenarios
- Linux, macOS, and Windows package smoke tests
- credential-gated live MiniMax autonomous coding scenarios

Coverage and adversarial tests are independent requirements: line execution does not replace explicit containment, rollback, conflict, or credential-protection assertions.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --locked --no-deps
cargo deny check advisories sources
cargo audit
```

Release-level checks are defined in [`.github/workflows/release-gates.yml`](.github/workflows/release-gates.yml).

## Documentation

- [Release and installation](docs/RELEASE.md)
- [Security hardening](docs/SECURITY-HARDENING.md)
- [Observability](docs/OBSERVABILITY.md)
- [Compatibility](docs/compatibility.md)
- [Contributing](CONTRIBUTING.md)

## License

Medusa is licensed under the [MIT License](LICENSE).
