# Durable patch transactions

Medusa's structured `patch_apply` and `symbol_rename` tools use a durable local journal under `.medusa/patch-transactions/`.

Each transaction records the exact pre-edit bytes, staged post-edit bytes, file permissions, changed paths, and state transitions before repository files are replaced. Journal state is synced after preparation and after each applied path.

The active agent runtime recovers non-terminal transactions before creating or loading a session. Recovery runs newest-first and restores only paths that were recorded as applied. Verification success promotes applied journals to committed; verification failure restores the exact pre-transaction state.

Formatting may legitimately change a file after the structured edit, so successful verification is the commit authority rather than an exact staged-byte comparison. The bounded verification evidence records the committed or rolled-back transaction identifiers.

Terminal journals remain available as local evidence. They are ignored by later recovery passes, making startup recovery idempotent.

This boundary currently covers structured multi-file edits only. Direct `fs_write`, directory creation, shell-side mutations, and Desktop Commander writes are handled by their existing safeguards and are the next unification slice.
