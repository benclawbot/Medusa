# Dependency hygiene evidence

## Scope

This document records the conservative dependency-pruning increment delivered through PR #52. The goal was to remove proven-unused direct manifest edges without refreshing unrelated transitive versions or claiming build improvements that the resolved graph does not support.

The complete production source of the two affected crates was inspected before changing their manifests:

- `medusa-browser-client` uses `medusa-core`, `serde`, and `serde_json`; it does not use its former direct `thiserror` or `ulid` declarations.
- `medusa-browserd` uses `medusa-browser-client`, `serde_json`, and `url`; it does not use its former direct `medusa-core`, `serde`, or `thiserror` declarations.

The lockfile was preserved from the base branch and updated only for the five workspace-package dependency entries. A full lock regeneration was explicitly rejected after metrics showed that it would add unrelated transitive versions.

## Exact before and after metrics

The permanent dependency-policy job measures the pull-request head and base with locked Cargo metadata and `cargo tree -d --locked`.

| Metric | Before | After | Delta |
|---|---:|---:|---:|
| Workspace packages | 18 | 18 | 0 |
| Direct dependency edges | 129 | 124 | -5 |
| Normal direct edges | 113 | 108 | -5 |
| Development direct edges | 16 | 16 | 0 |
| External direct edges | 92 | 88 | -4 |
| Locked packages | 297 | 297 | 0 |
| Resolved packages | 297 | 297 | 0 |
| Registry packages | 279 | 279 | 0 |
| Duplicate package names | 10 | 10 | 0 |
| Duplicate extra versions | 11 | 11 | 0 |
| Enabled feature selections | 632 | 632 | 0 |
| Packages with enabled features | 169 | 169 | 0 |

Removed external edges:

- `medusa-browser-client` → `thiserror`
- `medusa-browser-client` → `ulid`
- `medusa-browserd` → `serde`
- `medusa-browserd` → `thiserror`

Removed internal workspace edge:

- `medusa-browserd` → `medusa-core`

No direct edge was added.

## Build impact

The resolved package set, registry package count, duplicate-version count, and enabled feature selections are unchanged. Therefore this increment improves manifest ownership and reduces accidental coupling, but does **not** claim a smaller download graph, a smaller compiled package set, or a measurable clean-build speedup.

The practical benefits are narrower:

- each browser crate declares only libraries it directly uses
- future source changes cannot accidentally rely on undeclared transitive availability hidden by stale manifest entries
- dependency reviews have five fewer direct relationships to audit
- exact base/current graph metrics now run on every pull request in the existing read-only dependency-policy job

## Measurement tooling

`scripts/dependency-metrics.py` uses only Python's standard library and Cargo's own locked metadata. It reports:

- workspace and external direct edges by dependency kind
- locked, resolved, and registry package counts
- duplicate package names and extra versions
- enabled feature selections
- per-crate external direct dependencies
- the full `cargo tree -d --locked` duplicate report

The CI comparison is published in the dependency-policy job summary and retained as the `dependency-metrics` artifact. Metrics failures have their own diagnostic artifact and cannot be confused with cargo-deny or cargo-audit failures.

## Further pruning policy

Further dependency removal must remain evidence-driven. Large or platform-sensitive dependencies such as HTTP clients, SQLite, image codecs, clipboard backends, and tree-sitter parsers should not be removed or feature-pruned solely because a text search appears empty. Their source use, macros, test targets, platform gates, feature union, resolved graph, and package impact must be measured first.

A future increment is worthwhile only when it produces at least one of these outcomes without behavior loss:

- another proven-unused direct edge
- a resolved package reduction
- fewer duplicate versions
- fewer enabled features
- a documented platform-specific dependency reduction

`cargo deny check advisories sources` and `cargo audit` remain mandatory gates regardless of graph size.
