# Restored Skill Probation

Medusa evaluates restored approved skills against the evidence that originally triggered their lifecycle review. Probation is observational: it never edits, disables, deletes, or re-quarantines a skill automatically.

## Lifecycle

1. A weak approved skill receives a calibrated review recommendation.
2. An operator explicitly quarantines it with `medusa skills quarantine NAME --confirm`.
3. An operator explicitly restores it with `medusa skills restore NAME --confirm`.
4. The restored skill's existing `lifecycle.json` remains inside its active skill directory.
5. Every completed Medusa session refreshes `.medusa/learning/skill-probation/summary.json`.
6. The report compares post-restore outcomes with the recommendation baseline stored in `lifecycle.json`.

Only active skill directories containing both `SKILL.md` and a lifecycle record with `status: restored` are included.

## Commands

```text
medusa skills probation
medusa skills probation --json
medusa skills probation NAME
medusa skills probation NAME --json
```

The table view reports:

- lifecycle state;
- post-restore session and verified-session counts;
- post-restore verification rate;
- verification-rate change versus the pre-quarantine baseline;
- samples still required;
- the recommended operator action.

JSON mode exposes the complete policy and report schema for automation.

## Evidence policy

Probation requires three post-restore observations before it makes a stable comparison.

- `collecting`: fewer than three post-restore sessions.
- `passed`: verification rate is at least 75% and calibrated confidence is not below the original recommendation confidence.
- `failed`: verification rate is below 50% after the minimum sample count.
- `watch`: enough evidence exists, but the skill is neither clearly recovered nor clearly failing.

Confidence uses the same conservative prior as approved-skill effectiveness metrics: two prior verified observations out of four prior observations.

The baseline verified-session count is reconstructed from the durable recommendation's observed-session count and verification rate. All subtraction is saturating so malformed or stale historical counts cannot underflow.

## Storage

The aggregate report is written atomically to:

```text
.medusa/learning/skill-probation/summary.json
```

The report includes:

- baseline observations, rate, and confidence;
- post-restore observations, verified observations, rate, and confidence;
- signed verification-rate change;
- remaining sample count;
- restore timestamp and latest outcome timestamp;
- state and operator recommendation.

Malformed lifecycle files are skipped rather than blocking session persistence. Missing metrics produce a `collecting` report with zero post-restore observations.

## Safety properties

- No automatic quarantine or deletion.
- No skill content rewriting.
- No trust change before the minimum sample count.
- Deterministic ordering by skill name.
- Atomic summary replacement.
- Existing non-restored lifecycle records are ignored.
- The operator retains final authority through the explicit quarantine and restore commands.
