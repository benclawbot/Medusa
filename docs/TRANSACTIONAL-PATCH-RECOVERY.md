# Transactional patch recovery

Structured `patch_apply` and `symbol_rename` mutations are durably journaled under `.medusa/patch-transactions` before repository files are replaced.

Lifecycle:

1. validate all edits and stage complete before/after file images;
2. persist a `prepared` journal and fsync it;
3. record `applying` and each applied path as replacement progresses;
4. leave the transaction `applied` while repository verification runs;
5. promote it to `committed` after successful verification, or restore backups in reverse transaction order after failure;
6. on session creation or load, recover every non-terminal transaction to its exact pre-apply state.

Journal directories are ordered by their timestamp-prefixed transaction identifiers. Recovery and failed-verification rollback process newest transactions first so repeated edits to the same file restore the original pre-run state. Completed and rolled-back journals are terminal and ignored by repeated recovery.

This lifecycle currently covers the structured `patch_apply` and `symbol_rename` tools. Direct `fs_write`, directory creation, shell-side writes, and external MCP writes retain their existing policies and are not silently represented as journaled transactions.
