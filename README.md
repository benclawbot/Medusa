# Medusa

Medusa is a production-grade autonomous CLI coding agent implemented in Rust from the version 1.1.0 implementation contract recorded in [`MEDUSA_SPEC.md`](MEDUSA_SPEC.md).

## Current milestone

Phase 1 is complete and provides a working single-agent vertical slice:

- CLI repository bootstrap, search, guarded shell, and Git checkpoints;
- a provider-neutral model boundary and MiniMax-M3 Anthropic-compatible adapter;
- strict model tool schemas and validated built-in filesystem, search, shell, and Git execution;
- checksummed session persistence with restart and resume support;
- deterministic targeted verification and exact evidence capture;
- an end-to-end fixture that is inspected, fixed after a simulated restart, and verified.

Implementation proceeds automatically through the remaining phases. Each phase is independently tested, reported, committed, pushed, and merged to `main`; execution pauses only for a genuine external or safety blocker.

## Development checks

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cargo deny check advisories sources
cargo audit
```

See [`FINAL.md`](FINAL.md), [`docs/phase-1-evidence.md`](docs/phase-1-evidence.md), [`CONTRIBUTING.md`](CONTRIBUTING.md), and [`docs/compatibility.md`](docs/compatibility.md).
