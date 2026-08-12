# Tool output adapters

Medusa routes verbose developer-command output through the existing `medusa-agent::output_envelope` boundary before returning it to the model.

## Modes

The adapter contract supports three deterministic modes:

- `compact` removes ANSI/control noise, groups duplicate lines, preserves failures, keeps bounded beginning/end context, and reports omission metadata.
- `normal` applies the same safety-preserving transformations with a larger retained budget.
- `verbatim` returns the cleaned complete output without grouping or truncation.

The shipped `shell_run` path currently selects `compact` by default. The mode contract is public so repository policy and future native-tool routing can select `normal` or `verbatim` without introducing another output representation.

## Command-aware behavior

Shell output is adapted after sandboxed command execution and before it becomes model-visible evidence. The adapter:

- preserves the exact invoked program and arguments;
- records success or failure explicitly;
- strips ANSI sequences and carriage-return progress frames;
- deduplicates repeated lines while retaining occurrence counts;
- preserves error, failure, panic, assertion, denial, conflict, and timeout lines;
- retains surrounding context for failed commands;
- applies bounded Git-specific limits for status, log, and diff output;
- reports original, omitted, and duplicate-line counts;
- emits a deterministic expansion handle when content was compacted.

An expansion handle identifies the original command/output fingerprint and directs the caller to repeat the same operation with `output_mode=verbatim` when that mode is exposed by the calling policy. It never implies that omitted content was discarded from an artifact that had already been persisted.

## Safety and correctness

Compression is deterministic and never changes command exit status. Failed command output remains a failed `MedusaError`, with compacted failure evidence attached. The adapter retains failure lines before applying head/tail limits, so successful chatter is discarded before action-critical diagnostics.

The legacy artifact envelope remains responsible for durable full-body files where a tool has a session artifact root. Output adaptation and durable artifact persistence share one module but remain separate operations.

## Production wiring

The active path is:

`medusa-runtime::run_prompt -> medusa-agent::AgentEngine -> ToolManager -> shell_run -> sandboxed_command -> output_envelope::adapt_command`

This is the shipped single-agent execution path. The adapter is not an unused helper, metadata-only dependency, or architecture-policy exclusion.
