# Refactor Benchmark Contract

The modularization program must remain behavior-preserving and avoid material performance regressions.

## Frozen scenarios

Measure the following scenarios on the same machine, toolchain, build profile, repository fixture, and warm/cold state:

1. `cargo test --workspace --all-features --no-run`
   - captures workspace compile and link cost;
2. `cargo test -p medusa-agent`
   - captures orchestration and tool-policy test execution;
3. `cargo test -p medusa-memory`
   - captures Markdown persistence, indexing, retrieval, and lifecycle behavior;
4. `cargo test -p medusa-intelligence`
   - captures parsing, patching, formatting, and impact analysis;
5. `cargo build --release --locked --bin medusa`
   - captures release-build cost and final binary size;
6. `bash scripts/package-smoke.sh`
   - captures startup and help/version smoke behavior.

## Measurement method

Run each command at least five times after one untimed warm-up. Record wall-clock duration, peak resident memory where the platform exposes it, and output binary size for release builds. Compare medians rather than single runs.

A portable timing example is:

```bash
/usr/bin/time -p cargo test -p medusa-agent
/usr/bin/time -p cargo test -p medusa-memory
/usr/bin/time -p cargo test -p medusa-intelligence
/usr/bin/time -p cargo build --release --locked --bin medusa
wc -c target/release/medusa
```

On macOS, use `/usr/bin/time -l`; on Linux, `/usr/bin/time -v` may be used for memory evidence.

## Acceptance threshold

A median regression greater than 5% requires:

- the raw before/after measurements;
- an explanation of the cause;
- evidence that the regression is not measurement noise;
- explicit approval in the pull request.

Improvements do not permit weakening correctness, adversarial, coverage, or migration gates.

## Future upgrade

Where stable microbenchmarks are useful, add Criterion benchmarks for parsing, retrieval scoring, patch validation, and policy normalization. Do not add synthetic benchmarks that bypass the production path merely to report favorable numbers.
