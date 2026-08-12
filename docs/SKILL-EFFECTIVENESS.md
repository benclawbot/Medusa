# Approved skill effectiveness metrics

Medusa records outcome evidence for automatically loaded approved workspace skills, aggregates it into a deterministic effectiveness summary, calibrates confidence conservatively, and creates durable review recommendations when enough evidence shows persistent weakness.

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

Advisory review recommendations are rebuilt at:

```text
.medusa/learning/skill-reviews/recommendations.json
```

All writes are repository-local. Incomplete sessions and sessions without approved `.medusa/skills/*/SKILL.md` entries do not contribute samples.

## Metrics

Each skill contains:

- `observed_sessions`: completed sessions where the approved skill was automatically loaded.
- `verified_sessions`: observed sessions containing verification evidence.
- `verification_rate_milli`: verified sessions divided by observed sessions, scaled from 0 to 1000.
- `confidence_milli`: a smoothed confidence estimate scaled from 0 to 1000.
- `evidence_state`: `collecting`, `healthy`, `watch`, or `review`.
- `review_recommended`: whether Medusa recommends human review.
- `recommendation_reason`: the evidence behind a review recommendation.
- `average_turns_milli`: average agent turns, scaled by 1000 to avoid floating-point persistence.
- `average_evidence_milli`: average verification evidence entries, scaled by 1000.
- `latest_recorded_at`: latest included outcome timestamp.

Metrics are observational evidence, not proof of causality. Several approved skills can be loaded in one session, so each receives the same session outcome.

## Confidence calibration

Raw success rates are unstable with small sample sizes. Medusa therefore applies a fixed prior equivalent to two verified sessions out of four observations:

```text
confidence = (verified_sessions + 2) / (observed_sessions + 4)
```

The result is persisted as `confidence_milli`. A new skill begins at 500 rather than zero or one thousand, and repeated evidence gradually dominates the prior.

The policy is included in `summary.json` so future schema consumers do not need to infer thresholds from code.

## Evidence states

Medusa uses these deterministic states:

- `collecting`: fewer than five observed sessions. No review recommendation is emitted.
- `healthy`: at least five observations and a raw verification rate of 75% or higher.
- `watch`: at least five observations and a raw verification rate from 50% through 74.9%.
- `review`: at least five observations and a raw verification rate below 50%.

A `review` state creates an entry in `skill-reviews/recommendations.json` with the skill name, sample count, raw rate, calibrated confidence, reason, and latest observation timestamp.

## Safety boundary

Recommendations are advisory. This feature does not:

- remove or modify `SKILL.md` files;
- stop automatic loading;
- alter proposal approval history;
- deactivate, quarantine, or roll back a skill.

Those actions require an explicit later lifecycle feature with its own safeguards and review path.

## Determinism and resilience

Outcome files are processed in sorted path order, skill metrics are emitted in sorted name order, and recommendations are sorted by skill name. Re-recording an existing completed session is idempotent. Malformed outcome files are ignored during aggregation so they cannot break normal session persistence; valid records continue to contribute.
