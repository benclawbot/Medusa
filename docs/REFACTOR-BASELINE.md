# Refactor Baseline

This document freezes the measurable starting point for the post-1.0 modularization program.

## Baseline commit

The baseline is the `main` commit immediately after PR #10, `4ae6b11a9960300482bad1b34ea77d4871000365`.

## Required release state

The baseline release-gate matrix is green for:

- workspace coverage at the enforced 75% non-regression floor;
- named adversarial regressions;
- fuzz smoke tests;
- security checks;
- migrations and chaos recovery;
- documentation and schema checks;
- Linux, macOS, and Windows package smoke tests;
- live MiniMax autonomous coding scenarios.

The measured workspace line coverage at the start of the program is approximately 75.46%. Refactor pull requests must not reduce it below 75%.

## Production source-size policy

All Rust production sources under `crates/*/src/**/*.rs` are limited to 800 physical lines.

Temporary legacy exceptions must be registered in `docs/source-size-exceptions.txt` with:

1. the exact repository path;
2. a hard maximum that cannot grow;
3. a removal rationale tied to the delivery sequence.

New exceptions require explicit review. Deleting or splitting an excepted file must also remove its registry entry; stale entries fail CI.

## Public API freeze

The first extraction series is behavior-preserving. Public items, command names, tool schemas, serialized records, error codes, and user-visible output remain compatible unless a separate migration note is approved.

The compatibility contract is documented in `docs/PUBLIC-API-BASELINE.md`. Every extraction PR must run the full workspace test suite and release gates. Any intentional API change must include:

- a migration note;
- compatibility tests;
- an updated API baseline;
- an explicit rationale in the pull request.

## Performance baseline

The benchmark contract is documented in `docs/BENCHMARKS.md`. The current program uses reproducible command-level measurements until a dedicated Criterion suite is added.

No refactor may regress the frozen scenarios by more than 5% without an explicit explanation and approval.

## Verification commands

```bash
bash scripts/check-source-size.sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Release-level verification remains defined by `.github/workflows/release-gates.yml`.
