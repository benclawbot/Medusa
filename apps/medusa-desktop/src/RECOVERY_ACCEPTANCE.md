## Operator acceptance scenario

1. Open a repository containing a persisted recoverable session.
2. Verify the recovery dialog identifies the durable step and interrupted operation.
3. Inspect checkpoint metadata and the file-level restore preview.
4. Verify restore remains disabled until destructive effects are confirmed when local work conflicts.
5. Execute a valid recovery action and verify the runtime persists its audit record.
