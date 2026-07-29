# Learning UX acceptance matrix

This matrix is exercised by crate tests, desktop type checks, and the cross-platform GitHub Actions workspace suites.

- Explicit correction becomes a structured signal: `correction_signals` fixtures.
- Root cause and generalized lesson are proposed: `lesson_inference` fixtures.
- Correct solution type and scope are selected: `solution_selection` fixtures.
- Original failure is reproduced and resolved: `regression_replay` fixtures.
- User review lifecycle is authoritative: `learning_review::lifecycle_is_persistent_and_audit_chain_is_valid`.
- Activation survives restart: the file-backed review and scoped-memory stores are reopened in tests.
- Matching and non-matching retrieval: `retrieval` precision/recall and suppression fixtures.
- Conflicting feedback does not silently replace behavior: correction, scope-resolution, retrieval, and lifecycle conflict tests.
- Harmful behavior can be suspended or rolled back: lifecycle and guarded-promotion tests.
- Deletion and export follow privacy policy: learning-review redaction, tombstone, and audit-chain tests.
- Offline operation: lifecycle state and export use repository-local files only.
- Concurrent/stale clients fail safely: optimistic revision tests.
- Corrupted or tampered audit records fail safely: audit-chain verification.
- Sensitive microphone, image, credential, and secret fixtures fail closed before persistence or export.
- Desktop and TUI call the same `medusa_runtime::learning_review` API.
- Desktop controls are keyboard reachable, have dialog/filter labels, support narrow windows, and disable motion under reduced-motion preferences.
