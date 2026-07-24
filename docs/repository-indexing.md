# Repository indexing

Medusa maintains a deterministic, syntax-aware repository index for Rust source files. Generated, vendor, build, metadata, and binary paths are excluded by the shared source-discovery policy.

## Lifecycle

1. The first model turn for a repository builds a `CodeIndex` and matching `IndexSnapshot` in the process-wide cache.
2. Before every later model request, Medusa captures a new snapshot and compares it with the cached snapshot.
3. Added and modified files are reparsed; removed files are deleted from definitions, references, and parse-error state.
4. Unchanged repositories produce no refresh report and no visible activity.
5. Repository identities are isolated by path, so switching repositories never reuses another repository's index.

## Frontend visibility

When a refresh changes indexed state, the agent emits a normal `code_index` tool activity before the model request. The activity lists reindexed paths, removed paths, and files that still contain parse errors. TUI and Desktop receive the same event through the shared agent observer pipeline.

## Current language support

The syntax-aware extractor currently supports Rust through Tree-sitter. The snapshot, invalidation, deterministic ordering, and retrieval-budget primitives are language-neutral; additional language extractors remain part of issue #135.

## Related implementation

- `crates/medusa-intelligence/src/snapshot.rs`: deterministic snapshots and deltas.
- `crates/medusa-intelligence/src/index.rs`: full builds and incremental refreshes.
- `crates/medusa-agent/src/session_browser.rs`: repository-owned cache primitive.
- `crates/medusa-agent/src/repository_index.rs`: process-wide turn refresh coordination.
- `crates/medusa-agent/build.rs`: injects refresh before each generated engine model request.
