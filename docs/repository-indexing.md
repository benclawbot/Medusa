# Repository indexing

Medusa maintains a deterministic, syntax-aware repository index for Rust and Python source files. Generated, vendor, build, virtual-environment, metadata, cache, and binary paths are excluded by the shared source-discovery policy.

## Lifecycle

1. The first model turn for a repository builds a `CodeIndex` and matching `IndexSnapshot` in the process-wide cache.
2. Before every later model request, Medusa captures a new snapshot and compares it with the cached snapshot.
3. Added and modified files are reparsed with the parser selected from their file extension; removed files are deleted from definitions, references, and parse-error state.
4. Unchanged repositories produce no refresh report and no visible activity.
5. Repository identities are isolated by path. Changes to Git `HEAD`, its resolved reference, `packed-refs`, or `FETCH_HEAD` force a complete reload for branch, fetch, pull, and linked-worktree transitions.

## Prompt allocation and retrieval

Before each model request, Medusa accounts for system instructions, durable conversation, tool schemas, approval and memory context, and reserved response capacity. Repository retrieval may use only the remaining capacity below the proactive-compaction threshold.

The retrieval allocation is capped at 8,000 estimated tokens and keeps a fixed 256-token wrapper reserve. Ranked fragments are selected with the deterministic `RetrievalBudget` contract. The request is then budgeted again after repository context is appended, so retrieval cannot silently starve protected prompt sections or response capacity.

The retrieval query uses the current durable session objective. Included fragments carry their path, symbol, line range, score, and source content. Candidates that do not fit retain explicit exclusion reasons such as total-budget exhaustion, per-result limits, stale ranges, unavailable source, or result-count limits.

## Frontend visibility

When a refresh changes indexed state, the agent emits a normal `code_index` tool activity before the model request. The activity lists reindexed paths, removed paths, and files that still contain parse errors.

When repository context is selected, the agent emits a `repository_context` activity with included and excluded fragment counts, used and allocated retrieval tokens, protected capacity, and grouped exclusion reasons. TUI and Desktop receive both activities through the same shared agent observer pipeline.

## Current language support

- Rust: functions, structs, enums, traits, modules, type aliases, constants, statics, macros, and identifier references.
- Python: functions, methods, classes, and identifier references.

Both languages use deterministic path/source ordering, the same incremental invalidation path, and the same parse-error reporting contract. JavaScript, TypeScript, and other language extractors remain follow-up work in issue #135.

## Related implementation

- `crates/medusa-intelligence/src/snapshot.rs`: deterministic snapshots and deltas.
- `crates/medusa-intelligence/src/index.rs`: language dispatch, full builds, and incremental refreshes.
- `crates/medusa-intelligence/src/retrieval.rs`: ranking, hard budgets, and exclusion reasons.
- `crates/medusa-agent/src/session_browser.rs`: repository-owned cache primitive.
- `crates/medusa-agent/src/repository_index.rs`: process-wide refresh, Git identity coordination, retrieval formatting, and status reporting.
- `crates/medusa-agent/build.rs`: injects refresh, retrieval allocation, and request re-budgeting before each generated engine model request.
