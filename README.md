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
- **Persistent memory** — Markdown-first memory with validation, indexing, retrieval, lifecycle management, and provenance controls.
- **Parallel workers** — isolated worktrees, deterministic merge behavior, conflict detection, and cleanup safeguards.
- **Extensions and browser evidence** — skills, hooks, MCP isolation, Playwright-based browser verification, output redaction, and checksummed provenance.
- **Production hardening** — migrations, rollback bundles, observability, archive safety, fuzzing, chaos recovery, dependency policy, package smoke tests, and live-provider validation.

## Requirements

- Rust 1.88 or newer
- Cargo and Git
- `MINIMAX_API_KEY` for live MiniMax execution
- Node.js 22 when browser verification is enabled

## Installation

```bash
cargo install --path crates/medusa-cli --locked
medusa doctor
```

For local development:

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
| `Ctrl+C` | Cancel the active agent task |
| `Ctrl+D` | Exit when the composer is empty |
| `Esc` | Exit the terminal interface |

Prompt drafts and clipboard attachments are persisted under the repository's `.medusa` state directory until submission.

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
