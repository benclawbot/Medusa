# Medusa

Medusa is a production-grade autonomous CLI coding agent written in Rust. It combines a persistent local runtime, durable Markdown memory, repository-aware tools, sandboxed execution, multi-worker coordination, browser and extension support, release hardening, and live MiniMax coding validation.

The implementation contract is documented in [`MEDUSA_SPEC.md`](MEDUSA_SPEC.md). The shipped architecture and verification evidence are summarized in [`FINAL.md`](FINAL.md).

## Current status

The 1.0 implementation and production-hardening phases are complete and merged to `main`.

The current engineering focus is a behavior-preserving modularization refactor. The goal is to reduce large single-file crates into cohesive modules without weakening public APIs, sandbox boundaries, migration safety, adversarial regressions, packaging, or live-provider behavior.

See [`docs/NEXT-STEPS.md`](docs/NEXT-STEPS.md) for the crate-by-crate extraction plan, acceptance criteria, coverage policy, and delivery sequence.

## Core capabilities

- Autonomous repository inspection, planning, editing, verification, and iteration
- Persistent Markdown-first memory with validation, retrieval, and lifecycle management
- Sandboxed shell execution with path, environment, and network boundaries
- Guarded multi-file patch transactions with rollback and formatter integration
- Tree-sitter-backed code intelligence and test-impact selection
- Parallel worker execution with deterministic merge and conflict handling
- Skills, hooks, MCP isolation, browser evidence, and output redaction
- Reversible migrations, chaos recovery, observability, release manifests, and package validation
- Cross-platform release packaging for Linux, macOS, and Windows
- Credential-gated live MiniMax coding end-to-end validation

## Release gates

Pull requests are validated by independent gates for:

- full-workspace tests and documentation
- workspace line coverage with a 75% non-regression floor
- named adversarial security regressions
- fuzz, migration, and chaos scenarios
- dependency and credential-safety checks
- Linux, macOS, and Windows packaging
- live MiniMax autonomous coding behavior

Coverage and adversarial behavior are intentionally separate requirements. A high execution percentage cannot substitute for explicit containment, rollback, conflict, or secret-protection tests.

## Development checks

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cargo deny check advisories sources
cargo audit
```

For release-level verification, also run the workflows defined in [`.github/workflows/release-gates.yml`](.github/workflows/release-gates.yml).

## Repository map

- `crates/medusa-agent` — orchestration, sessions, tools, policy, and verification
- `crates/medusa-intelligence` — parsing, indexing, patching, formatting, and impact analysis
- `crates/medusa-memory` — Markdown memory, validation, indexing, retrieval, and lifecycle
- `crates/medusa-extensions` — skills, hooks, MCP, browser integration, and redaction
- `crates/medusa-hardening` — migrations, observability, release validation, archives, and chaos fixtures
- `crates/medusa-workers` — parallel worktrees, merge coordination, and conflict handling
- `crates/medusa-provider` — model-provider integration, including MiniMax
- `crates/medusa-cli` — command-line entry point
- `crates/medusa-daemon` and `crates/medusa-tui` — persistent runtime and terminal interface

## Documentation

- [`MEDUSA_SPEC.md`](MEDUSA_SPEC.md) — implementation contract
- [`FINAL.md`](FINAL.md) — final implementation report
- [`docs/NEXT-STEPS.md`](docs/NEXT-STEPS.md) — modularization roadmap
- [`docs/RELEASE.md`](docs/RELEASE.md) — release process
- [`docs/SECURITY-HARDENING.md`](docs/SECURITY-HARDENING.md) — security controls
- [`docs/OBSERVABILITY.md`](docs/OBSERVABILITY.md) — operational evidence
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — contribution workflow
