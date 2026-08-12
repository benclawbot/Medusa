# Medusa performance benchmark contract

This directory defines the first durable measurement contract for issue #693.

The primary metric is elapsed time from an accepted objective to authoritative verified completion on the accepted repository result. Runs must not report an earlier unverified edit as completion.

## Required run modes

Record cold and warm runs separately. Every result identifies the repository revision, scenario, platform, machine profile, provider mode, verification outcome, and phase timings.

## Comparison rules

A candidate fails when it is faster only because verification coverage or task success regressed. Failed and timed-out runs remain in the dataset. Baseline updates require an explicit reviewed change with before/after artifacts.

Use `python scripts/performance/compare_runs.py BASELINE.json CANDIDATE.json` to compare two result files. The comparator is deterministic, uses only the Python standard library, rejects incomplete results, and exits non-zero on material latency or correctness regressions.
