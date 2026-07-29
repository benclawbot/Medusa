# Durable tool output expansion

Medusa persists shell output only when the command-aware adapter omits or deduplicates content.

## Production path

The active execution path is:

`medusa-runtime -> AgentEngine -> ToolManager -> shell_run -> sandboxed_command -> output adapter -> expansion persistence`

The compact output returned to the model contains both the deterministic expansion handle and a repository-relative path such as:

`.medusa/output-expansions/<digest>.txt`

The model retrieves the exact raw command, stdout, and stderr through the existing read-only `fs_read` tool. No second retrieval subsystem or architecture-policy exemption is used.

## Properties

- Expansion files are created only when output was omitted or deduplicated.
- Paths are derived from the same SHA-256 input used by the adapter handle.
- Identical raw output reuses the same file.
- Files stay inside the repository sandbox.
- Failed commands preserve their failure status while retaining full diagnostics for later retrieval.
- Compact output remains the default model-visible representation.

Further issue #529 work will add explicit per-call output mode selection across tool schemas, retention and cleanup policy, cache invalidation, telemetry, and benchmarks.
