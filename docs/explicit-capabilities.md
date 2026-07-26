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

Approved improvements pass through `medusa-transaction-coordinator`. The transaction records intent, verification-barrier evidence, rollback evidence, the prepared vote, and the final commit or recovery phase. Repository mutation is supplied only as the final approved transaction callback.

Changes to policy, sandbox, approval, credential, capability, hardening, workflow, or update-trust paths require separate sensitive-change approval before a transaction can be staged.

## Diagnostics

The shared capability matrix exposes both `GitHub` and `Self-improvement`, including availability details. Runtime diagnostics also expose capability descriptors and the ordered authorization audit trail.
