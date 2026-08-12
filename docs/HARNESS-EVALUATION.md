# Harness Evaluation and Session Feedback

Medusa now exposes deterministic coding-task evaluation and durable completed-session feedback primitives from `medusa-hardening`.

## Evaluation gates

`CodingTaskOutcome` scores correctness, safety, scope adherence, diff quality, maintainability, recovery, planning, efficiency, and user burden on a 0–1000 scale. Correctness and safety have the largest weights and also have independent hard floors, so efficiency cannot compensate for an unsafe or incorrect candidate.

`compare_outcomes` requires the baseline and candidate to use the same task identifier and hidden-oracle digest. The default policy rejects aggregate regression and requires at least 850 correctness, 900 safety, and 750 weighted quality.

Hidden checks should be hashed with `oracle_digest`; the checks themselves do not need to be written into the evaluation report.

## Session feedback

`persist_session_feedback` accepts a normalized completed-session trajectory and writes an immutable, idempotent record under:

```text
.medusa/improvements/session-feedback/<session-id>.json
```

The record includes a weighted evaluation, source digest, and bounded improvement hints. Verification failures recommend better test discovery. Low-scoring sessions recommend a human-reviewed recovery heuristic change. Identical feedback can be recorded repeatedly without duplication; changed feedback for an already recorded session is rejected.

Frontends and runtimes should normalize their durable events into `TrajectorySignal` values and invoke the persistence API only after completion evidence has been finalized.

## Existing sandbox boundary

Shell execution remains enforced by the platform sandbox in `medusa-agent`: Linux uses bubblewrap, macOS uses a repository-scoped `sandbox-exec` profile with network denial, and unsupported platforms retain process containment and command-policy enforcement. Evaluation and feedback do not weaken these controls.
