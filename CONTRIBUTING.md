# Contributing

Implementation follows `MEDUSA_SPEC.md` section 31. Each phase must satisfy its own exit criteria and regress all previously passed criteria before the next phase starts.

Required checks:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Protocol and configuration changes must follow `docs/compatibility.md`.
