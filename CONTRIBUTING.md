# Contributing to Medusa

Thank you for improving Medusa. Changes should be narrowly scoped, behavior-focused, and backed by tests that demonstrate the intended result.

## Development setup

```bash
git clone https://github.com/benclawbot/Medusa.git
cd Medusa
cargo build --workspace --all-features --locked
```

Rust 1.88 or newer is required. Browser verification additionally requires Node.js 22 and the pinned Playwright Chromium package.

## Required checks

Run the same core checks used by CI before opening a pull request:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --locked --no-deps
cargo deny check advisories sources
cargo audit
```

The release workflow also enforces:

- at least 75% workspace line coverage
- named adversarial containment and rollback regressions
- fuzz, migration, and chaos scenarios
- Linux, macOS, and Windows package smoke tests
- credential-gated live MiniMax coding tests

## Change guidelines

- Preserve repository containment, sandbox, rollback, credential-redaction, and migration guarantees.
- Prefer small modules and narrow public APIs.
- Add meaningful assertions rather than tests that only execute lines.
- Keep serialized session, protocol, and configuration changes backward-compatible unless a migration is included.
- Never commit provider credentials, generated `.medusa` state, build outputs, or local test artifacts.
- Update user-facing documentation when commands, configuration, behavior, or compatibility changes.

Protocol and configuration changes must follow [`docs/PROTOCOL-VERSIONING.md`](docs/PROTOCOL-VERSIONING.md).

## Pull requests

A pull request should explain:

- the problem and intended behavior
- the implementation approach
- tests and evidence
- security or migration impact
- rollback considerations when state or release behavior changes

All required checks must pass before merge.
