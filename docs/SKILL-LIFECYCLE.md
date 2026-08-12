# Approved skill quarantine and restore

Medusa can turn a calibrated review recommendation into an explicit, reversible skill lifecycle action. It never quarantines a skill automatically.

## Commands

```text
medusa skills reviews
medusa skills reviews --json
medusa skills quarantine NAME --confirm
medusa skills quarantine NAME --confirm --reason "operator explanation"
medusa skills restore NAME --confirm
medusa --repo /path/to/repository skills reviews
```

`--confirm` is mandatory for both state-changing commands. A quarantine is accepted only when the current review queue contains a recommendation for the named skill.

## Review source

Review recommendations are produced by the confidence-calibration stage and stored at:

```text
.medusa/learning/skill-reviews/recommendations.json
```

`medusa skills reviews` presents the recommendation sample count, raw verification rate, calibrated confidence, and reason. JSON mode returns the persisted queue for scripts and future frontends.

## Quarantine behavior

An active approved skill lives at:

```text
.medusa/skills/NAME/SKILL.md
```

A confirmed quarantine atomically moves the complete skill directory to:

```text
.medusa/learning/skill-quarantine/NAME/
```

Because automatic retrieval only scans direct children of `.medusa/skills`, a quarantined skill stops loading immediately without deleting its instructions or provenance.

Medusa writes `lifecycle.json` beside the quarantined skill. The receipt records:

- the skill name and lifecycle status;
- original and quarantine paths;
- quarantine timestamp;
- operator reason, or the review recommendation reason when no override is supplied;
- the complete recommendation snapshot used to authorize the action;
- a future restore timestamp.

If the lifecycle receipt cannot be written after the move, Medusa attempts to move the directory back and reports whether rollback succeeded.

## Restore behavior

A restore requires `--confirm`, a valid quarantine receipt, and an empty active destination. Medusa refuses to overwrite any existing `.medusa/skills/NAME` directory.

The full directory is moved back, preserving `SKILL.md` and supporting files byte-for-byte. The lifecycle receipt remains with the restored directory and records the restore timestamp and `restored` status.

## Safety properties

- Skill names must be one normal path component; parent traversal and nested paths are rejected.
- Quarantine requires an active review recommendation.
- Neither quarantine nor restore overwrites an existing destination.
- Both actions are repository-local and remain auditable through durable receipts.
- No recommendation causes an automatic lifecycle change.
- Quarantine is reversible; deletion is outside this feature.
