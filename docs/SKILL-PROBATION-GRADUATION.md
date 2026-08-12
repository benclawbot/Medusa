# Skill probation graduation

A restored approved skill remains under probation until Medusa has collected enough post-restore evidence. A `passed` probation state is advisory evidence that the skill has recovered; graduation is the explicit operator decision that closes probation and returns the skill to ordinary trusted operation.

## Command

```text
medusa [--repo PATH] skills graduate NAME --confirm
```

Graduation always requires `--confirm`. Medusa refuses graduation when the named skill:

- has no durable probation report;
- is still `collecting` evidence;
- is in `watch` or `failed` state;
- lacks an active restored lifecycle record;
- has a mismatched lifecycle identity or status;
- already has a graduation receipt.

Only a probation report with state `passed` is eligible.

## Durable state

Graduation updates the active skill lifecycle record at:

```text
.medusa/skills/<name>/lifecycle.json
```

The lifecycle status changes from `restored` to `graduated`, and `graduated_at_epoch_seconds` is recorded. The original quarantine recommendation, reason, paths, quarantine timestamp, and restore timestamp remain preserved.

A separate immutable receipt is written to:

```text
.medusa/learning/skill-graduations/<name>.json
```

The receipt contains:

- the skill name;
- graduation timestamp;
- lifecycle record path;
- the complete `passed` probation report used for the decision;
- baseline and post-restore verification evidence;
- confidence and verification-rate changes.

The skill is removed from the active probation summary after the receipt is persisted.

## Safety and failure behavior

Graduation does not rewrite `SKILL.md`, delete evidence, or change automatic-loading behavior. It records an explicit lifecycle decision only.

Writes use temporary files followed by rename. If receipt creation fails, Medusa rolls the lifecycle record back to `restored`. Existing graduation receipts are never overwritten. Skill names containing nested paths or parent traversal are rejected.

Graduation is never automatic. A `passed` probation result remains advisory until an operator runs the confirmed command.
