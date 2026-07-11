# Medusa

Medusa is a production-grade autonomous CLI coding agent implemented in Rust from the version 1.1.0 implementation contract recorded in [`MEDUSA_SPEC.md`](MEDUSA_SPEC.md).

## Current milestone: Phase 0

Phase 0 establishes:

- Rust 2024 Cargo workspace and pinned compiler contract;
- versioned protocol and integrity-protected event envelopes;
- typed configuration with deterministic precedence and fail-closed validation;
- structured errors and stable identifiers;
- deterministic test fixtures;
- CI formatting, linting, tests, docs, dependency audit, and license checks.

Development follows strict block-and-report phase gates. Phase 1 does not begin until Phase 0 is reviewed and explicitly approved.

## Development checks

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

See [`FINAL.md`](FINAL.md), [`CONTRIBUTING.md`](CONTRIBUTING.md), and [`docs/compatibility.md`](docs/compatibility.md).
