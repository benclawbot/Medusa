# Medusa

Medusa is a production-grade autonomous CLI coding agent implemented in Rust from the version 1.1.0 implementation contract recorded in [`MEDUSA_SPEC.md`](MEDUSA_SPEC.md).

## Current milestone

Phase 1 is in active implementation on `agent/phase-1-vertical-slice` and covers:

- CLI entry points and repository bootstrap;
- filesystem search, guarded shell execution, and Git checkpoints;
- persistent, checksummed sessions with restart and resume behavior;
- the MiniMax-M3 provider boundary, agent loop, targeted verification, and end-to-end fixture gate.

Every phase is independently verified, reported, committed, pushed, and merged to `main`. The user has explicitly authorized automatic progression through all phases; execution pauses only for a genuine external or safety blocker.

## Development checks

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

See [`FINAL.md`](FINAL.md), [`CONTRIBUTING.md`](CONTRIBUTING.md), and [`docs/compatibility.md`](docs/compatibility.md).
