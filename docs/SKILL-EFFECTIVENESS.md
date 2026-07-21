# Approved skill effectiveness metrics

Medusa records outcome evidence for automatically loaded approved workspace skills and aggregates it into a deterministic effectiveness summary.

## Commands

```text
medusa skills metrics
medusa skills metrics --json
medusa --repo /path/to/repository skills metrics
```

The default report is tab-separated for quick terminal inspection. JSON mode returns the persisted schema unchanged for scripts and future frontends.

## Storage

Completed-session outcomes are stored under:

```text
.medusa/learning/skill-outcomes/<session-id>.json
```

The aggregate summary is rebuilt after each eligible completed session at:

```text
.medusa/learning/skill-metrics/summary.json
```

Both writes are repository-local. Incomplete sessions and sessions without approved `.medusa/skills/*/SKILL.md` entries do not contribute samples.

## Metrics

Each skill contains:

- `observed_sessions`: completed sessions where the approved skill was automatically loaded.
- `verified_sessions`: observed sessions containing verification evidence.
- `verification_rate_milli`: verified sessions divided by observed sessions, scaled from 0 to 1000.
- `average_turns_milli`: average agent turns, scaled by 1000 to avoid floating-point persistence.
- `average_evidence_milli`: average verification evidence entries, scaled by 1000.
- `latest_recorded_at`: latest included outcome timestamp.

Metrics are observational evidence, not proof of causality. Several approved skills can be loaded in one session, so each receives the same session outcome. Later learning-loop stages may use minimum sample sizes and comparative evidence before changing confidence or proposing deactivation.

## Determinism and resilience

Outcome files are processed in sorted path order and skill metrics are emitted in sorted name order. Re-recording an existing completed session is idempotent. Malformed outcome files are ignored during aggregation so they cannot break normal session persistence; valid records continue to contribute.

This feature only measures and reports effectiveness. It does not automatically alter, disable, or roll back a skill.
