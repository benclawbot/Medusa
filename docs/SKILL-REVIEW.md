# Skill review and approval

Medusa keeps generated skill proposals inactive under `.medusa/learning/skill-proposals/<name>/` until a user explicitly reviews them.

## Commands

```text
medusa [--repo PATH] skills list [--json]
medusa [--repo PATH] skills show NAME [--json]
medusa [--repo PATH] skills approve NAME
medusa [--repo PATH] skills reject NAME [--reason TEXT]
medusa [--repo PATH] skills scaffold NAME --program PATH [--runtime native_command|python|node]
medusa [--repo PATH] skills validate NAME [--json]
```

`list` summarizes proposal status, revision, confidence, approval state, and contributing lesson count. `show` prints the complete proposed `SKILL.md` and manifest, or the manifest alone as JSON.

Approval validates that:

- the proposal name cannot escape its proposal directory;
- the manifest name matches the selected proposal;
- the proposal is still awaiting review;
- the declared installation path is exactly `.medusa/skills/<name>/SKILL.md`;
- the source `SKILL.md` exists;
- no active skill already exists at the destination.

Only after all checks pass is the skill copied atomically into the project skill root. The proposal manifest is then marked `approved`, `requires_approval` becomes `false`, and `review_decision` records the decision.

Rejection never creates an active skill. It marks the proposal `rejected`, clears the approval requirement, and optionally records a review reason.

Approval deliberately refuses to overwrite an existing active skill. Replacing or updating an active skill remains a separate, explicit workflow.

Executable packages use `.medusa/skills/<name>/skill.json` plus `SKILL.md`, regular-file entrypoints and bounded tests/verification files. The manifest declares the runtime, typed JSON input/output schemas, capabilities, explicit environment-name declarations, repository and network scopes, side-effect/artifact paths, idempotency, cancellation support, and bounded CPU-time/memory/time/process/disk/output budgets. `scaffold` creates a reviewable package skeleton; `validate` is the explicit activation step that writes `skill.validation.json`. The `skill_execute` tool refuses packages without a current receipt, copies the package into a temporary contained workspace, and returns bounded output with a digest-bound provenance record. It never grants direct repository mutation access or ambient network/secrets.
