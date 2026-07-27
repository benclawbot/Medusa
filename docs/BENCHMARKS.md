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
