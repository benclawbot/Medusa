# Reliability and recovery benchmark contract

Medusa benchmarks product guarantees through the authoritative `cargo product-acceptance` suite. The benchmark layer scores retained acceptance evidence; it does not replace production runtime tests with synthetic mocks.

## Versioned suite

`benchmarks/reliability-suite.json` defines the benchmark schema, scenarios, run count, metrics, and release-blocking thresholds. Its representative tasks cover:

- small verified repair;
- multi-file bug fix;
- dependency or API migration;
- interrupted work and resume after runtime termination;
- failed verification and rollback;
- prohibited filesystem or network access;
- adversarial process spawning.

Platform-specific acceptance scenarios may satisfy one benchmark case through `platform_any`. A case passes only when at least one applicable authoritative scenario exists and every selected scenario passes.

## Deterministic execution

Run the credential-free suite with:

```bash
python3 scripts/reliability-benchmark.py
```

The command executes the product acceptance contract twice by default and writes:

- `target/reliability-benchmark/reliability-benchmark.json`;
- `target/reliability-benchmark/reliability-benchmark.md`;
- the original per-run acceptance summaries and logs.

The report records the exact commit, run count, scenario results, time to verified completion, and threshold failures. A test process exiting successfully is not sufficient: the underlying acceptance scenario must emit its required evidence marker and be recorded as `passed`.

## Metrics and release thresholds

The deterministic suite blocks release unless it records:

- 100% verified completion;
- 0% false completion;
- 100% successful resume;
- 100% successful rollback;
- zero containment violations;
- zero manual interventions;
- 100% repeated-run determinism.

Historical release reports belong under `benchmarks/results/<release-or-commit>/` and must retain both JSON and Markdown outputs with artifact provenance.

## Optional live-model evaluation

Live-provider evaluation is separate and never substitutes for deterministic runtime evidence. Every live result must record provider, model, complete non-secret configuration, exact commit, and run count. Credentials remain external and are never included in benchmark artifacts.

## Performance measurements

For refactor performance comparisons, measure the same machine, toolchain, build profile, fixture, and warm/cold state. Compare medians across at least five timed runs after one warm-up. A median regression greater than 5% requires raw measurements, a noise analysis, and explicit approval. Performance gains never permit weakening correctness, adversarial, coverage, migration, or recovery gates.

## End-to-end tool-orchestration benchmark

`benchmarks/orchestration-suite.json` and `scripts/orchestration-benchmark.py` score complete trajectories produced by the shipped `cargo product-acceptance` entry point. The scenarios cover unfamiliar navigation, repeated repository work, localized and cross-package fixes, dependency updates, failing-test diagnosis, large outputs, transient recovery, context-heavy work, safe parallelism, and compression recovery reads.

The report retains task success, first-pass success, median and p95 duration, critical-path latency, tool-call and redundancy counts, token classes, retained context, billed cost, cache behavior, fallback recovery, verification coverage, speculation waste, compression recovery reads, and user corrections. Only task success, verification coverage, and safety are release invariants; performance and cost results are reported as measured tradeoffs rather than compared with arbitrary preselected percentages.

Repository-specific orchestration learning is stored in `.medusa/orchestration-profile.json`. It is versioned, resettable, confidence-scored, time-decayed, and capped to a small recommendation-score adjustment. Explicit policy, requested output mode, permissions, containment, mutation serialization, verification requirements, and budget ceilings always take precedence. Missing, disabled, stale, corrupt, adversarial, or low-confidence profiles fail closed and contribute no learned adjustment.

Run the scoring contract without Rust:

```bash
python3 scripts/test-orchestration-benchmark.py
```

Run the complete benchmark through the shipped runtime acceptance entry point:

```bash
python3 scripts/orchestration-benchmark.py
```

## Same-model coding harness quality benchmark

`benchmarks/coding-harness-suite-v1.json` freezes the coding-harness corpus and feature matrix for issues #873 through #877. The corpus covers localized fixes, unfamiliar navigation, cross-module changes, regression tests, simultaneous diagnostics, architecture failures, dependency/configuration work, long-horizon repair loops, forced compaction/resume, repository drift, disproved hypotheses, broad-verification failures, blocked primary paths, truthful partial completion, and no-change controls.

The runner is deliberately downstream of the production acceptance authority:

```bash
python3 scripts/coding-harness-benchmark.py
```

It executes `cargo product-acceptance` twice by default and binds every report to the exact suite hash, suite/task version, repository revision, harness feature set, provider/model/configuration identity, and hashes of the authoritative verification receipts. Success is impossible when required verification is absent. Correctness, verification coverage, false completion, and safety are promotion guards; latency, token, context, repair, and cost measurements cannot override a correctness or safety regression.

The report includes task success, first-pass correctness, repair cycles, duplicate calls, deterministic retries, retained/reread context, verification coverage, stale-evidence incidents, roadblock recovery, wall-clock/tool latency, token/cost counters, manual intervention, final-diff evidence when emitted by the authoritative scenario, and exact receipt fingerprints. Metrics unavailable from a given production receipt are recorded as zero or unavailable rather than invented.

Feature variants are versioned as baseline, each of #873/#874/#875/#876/#877 independently, and the current cumulative production harness. Runs intended for comparison must use the same provider, model, non-secret configuration, frozen suite, and task revision. Compare retained reports with:

```bash
python3 scripts/compare-coding-harness-benchmarks.py BASELINE.json CANDIDATE.json
```

The comparison rejects model/configuration or corpus mismatches and enforces feature-specific assertions plus global correctness/safety guards. This allows historical or controlled harness revisions to be compared without task-specific prompt tuning.

The deterministic scoring contracts are independently testable:

```bash
python3 scripts/test-coding-harness-benchmark.py
python3 scripts/test-compare-coding-harness-benchmarks.py
```

Pull requests touching this benchmark run a production coding-harness baseline in `Reliability Benchmarks`; main-branch runs retain it alongside the reliability and orchestration evidence.

## TypeScript/JavaScript workspace benchmark

`crates/medusa-intelligence/benches/typescript_workspace.rs` measures deterministic production workspace discovery and content fingerprinting for 100, 1,000, and 5,000 supported source files. Every iteration verifies the exact source count and stable workspace fingerprint before emitting a machine-readable timing line. Generated and dependency paths are present in the fixture but excluded from adapter coverage.

Compile and run it with:

```bash
cargo bench -p medusa-intelligence --bench typescript_workspace --locked
```

The final `Code Intelligence Certification` workflow compiles the benchmark on Linux, macOS, and Windows and executes it on Linux. Results are performance evidence only; correctness, freshness, repository confinement, stale-state refusal, and cross-file mutation safety remain release invariants.
