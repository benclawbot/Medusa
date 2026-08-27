# Repository indexing

Medusa maintains a deterministic, syntax-aware repository index for Rust and Python source files. Generated, vendor, build, virtual-environment, metadata, cache, and binary paths are excluded by the shared source-discovery policy.

## Lifecycle

1. The first model turn for a repository builds a `CodeIndex` and matching `IndexSnapshot` in the process-wide cache.
2. Before every later model request, Medusa captures a new snapshot and compares it with the cached snapshot.
3. Added and modified files are reparsed with the parser selected from their file extension; removed files are deleted from definitions, references, and parse-error state.
4. Unchanged repositories produce no refresh report and no visible activity.
5. Repository identities are isolated by path. Changes to Git `HEAD`, its resolved reference, `packed-refs`, or `FETCH_HEAD` force a complete reload for branch, fetch, pull, and linked-worktree transitions.

## Prompt allocation and retrieval

Before each model request, Medusa accounts for system instructions, durable conversation, tool schemas, approval and memory context, and reserved response capacity. The runtime's request-specific repository projection is assembled once and passed to the agent as additional system context; the agent then budgets the final request and compacts if required.

The runtime projection is capped at 6,000 source tokens, 16 exact ranges, 1,200 tokens per range, and a 36 KiB rendered context. Ranked fragments are selected with the deterministic `RetrievalBudget` contract. The final request is budgeted after repository context is appended, so retrieval cannot silently starve protected prompt sections or response capacity.

The retrieval query combines the current prompt, durable session objective, and active plan. Included fragments carry their path, symbol, line range, score, and source content, with caller/reference ranges added when they fit. Candidates that do not fit retain explicit exclusion reasons such as total-budget exhaustion, per-result limits, stale ranges, unavailable source, or result-count limits.

## Frontend visibility

When a refresh changes indexed state, the agent emits a normal `code_index` tool activity before the model request. The activity lists reindexed paths, removed paths, and files that still contain parse errors.

The agent emits a normal `code_index` activity when its process-wide index changes. The selected runtime projection is included in the request manifest's additional-context fingerprint, so replay/audit can identify the exact context without maintaining a second injected repository projection.

## Current language support

- Rust: functions, structs, enums, traits, modules, type aliases, constants, statics, macros, and identifier references.
- Python: functions, methods, classes, and identifier references.

Both languages use deterministic path/source ordering, the same incremental invalidation path, and the same parse-error reporting contract. JavaScript, TypeScript, and other language extractors remain follow-up work in issue #135.

## Related implementation

- `crates/medusa-intelligence/src/snapshot.rs`: deterministic snapshots and deltas.
- `crates/medusa-intelligence/src/index.rs`: language dispatch, full builds, and incremental refreshes.
- `crates/medusa-intelligence/src/retrieval.rs`: ranking, hard budgets, and exclusion reasons.
- `crates/medusa-agent/src/session_browser.rs`: repository-owned cache primitive.
- `crates/medusa-agent/src/repository_index.rs`: process-wide refresh and Git identity coordination.
- `crates/medusa-runtime/src/repository_context.rs`: request-specific ranking, caller expansion, policy protection, rendering, and verification-scope selection.
