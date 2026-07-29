# Per-call tool output modes

Medusa's built-in `shell_run` tool accepts an optional `output_mode` argument:

- `compact` is the default and retains action-critical failures while reducing repetitive output.
- `normal` retains broader context with the same command-aware filtering and expansion metadata.
- `verbatim` returns cleaned raw command evidence without compact-mode omission or deduplication.

The tool schema exposes the accepted enum values and defaults to `compact` when the caller omits the field. The same parser is used for both ordinary execution and interactively approved execution, so authorization cannot bypass output-mode validation.

## Production path

```text
provider tool call
  -> ToolManager schema validation
  -> OutputMode::parse
  -> shell::run or shell::run_approved
  -> scheduler and execution budget
  -> repository cache keyed by output mode
  -> sandboxed command execution
  -> command-aware output adapter
  -> telemetry and proportional verification
  -> optional deterministic expansion persistence
```

Invalid values fail visibly as validation errors before command execution. Compact and normal results that omit or deduplicate content continue to expose deterministic expansion metadata and a repository-scoped path retrievable through `fs_read`.

Scheduler input and cache keys include the selected mode, so compact, normal, and verbatim executions cannot collide or reuse evidence produced under another output contract.
