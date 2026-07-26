# Explicit GitHub and self-improvement capabilities

`medusa-capabilities` is the shared authority for capability discovery, permission grants, explicit approvals, diagnostics, and audit events used by the CLI, TUI, desktop runtime, and agent prompt context.

## GitHub operations

GitHub reads and writes are routed through `medusa-github`. Read permission is required before authentication or repository queries execute. Writes are first represented as reviewable proposals and require an explicit approval before any `gh` or `git` command is invoked.

Each authorization decision records the capability, requested permission, approval state, outcome, and reason. Denied permissions fail before an external command or other side effect.

## Self-improvement

`medusa-improvement` remains proposal and evaluation logic; it does not receive a direct repository mutation path. A proposed improvement includes:

- evidence-backed rationale;
- a reviewable unified diff;
- verification commands;
- rollback metadata;
- touched paths and safety analysis.

The runtime derives changed paths from the submitted unified diff and requires them to exactly match the declared touched paths. Duplicate pending proposal IDs are rejected so reviewed work cannot be replaced under an existing transaction identifier.

Approved improvements pass through `medusa-transaction-coordinator`. A successful verification callback must provide evidence for every declared verification command before the prepared vote is cast. Repository mutation is supplied only as the final approved transaction callback, and a commit failure invokes an executable rollback callback before the transaction is marked failed.

Changes to policy, sandbox, approval, credential, capability, hardening, workflow, or update-trust paths require separate sensitive-change approval before a transaction can be staged. This includes the concrete approval, policy, Windows sandbox, and desktop credential implementations.

## Diagnostics

The shared capability matrix exposes both `GitHub` and `Self-improvement`, including availability details. Runtime diagnostics also expose capability descriptors and the ordered authorization audit trail. A shipped CLI entry point constructs the explicit runtime and emits these diagnostics as JSON.
