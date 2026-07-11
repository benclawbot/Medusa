# Medusa

Medusa is a production-grade autonomous CLI coding agent implemented in Rust from the version 1.1.0 implementation contract recorded in [`MEDUSA_SPEC.md`](MEDUSA_SPEC.md).

## Current milestone

Phases 0–2 are complete and merged. Phase 3 is in active implementation and adds:

- a persistent local daemon with Unix-socket JSON protocol;
- single-process ownership and durable job records;
- restart recovery for orphaned work;
- reconnect-safe clients while daemon-owned processes continue;
- a Ratatui terminal dashboard consuming the same protocol.

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
