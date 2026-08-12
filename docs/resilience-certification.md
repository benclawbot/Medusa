# Resilience certification

Medusa's resilience certification is a cross-platform gate for malformed input, state-machine invariants, deterministic recovery, and crash-resilience evidence. It composes the existing subsystem authorities instead of creating a second persistence, scheduling, process, or transaction implementation.

## Pull-request smoke gate

`.github/workflows/resilience-certification.yml` runs on Linux, macOS, and Windows when resilience-sensitive code changes. The bounded PR campaign uses `PROPTEST_CASES=256` and covers:

- deterministic fault/corruption fixtures from `medusa-testkit`;
- malformed protocol input and protocol/action state-machine properties;
- transaction coordinator, execution checkpoint, worker lease, process registry, time-travel, and session-continuity suites;
- the canonical `medusa-agent` journal corruption/crash-recovery tests.

The normal repository CI remains responsible for full workspace correctness. This workflow is a focused additional release-safety gate for authority and recovery boundaries.

## Scheduled and manual campaign

A daily scheduled run and `workflow_dispatch` execute the same cross-platform certification with `PROPTEST_CASES=4096` and a larger shrink budget. Property-test regression files are uploaded on failure so minimized reproducers can be retained and committed as regression seeds where appropriate.

## Deterministic fault plans

`medusa_testkit::resilience::FaultPlan` chooses failures solely from a recorded seed, fault point, and invocation number. Supported fault-point identities cover durable append/sync, event publication, process spawn/registration, cancellation, candidate promotion, verification receipts, snapshot persistence, and action delivery.

`corruption_cases` produces bounded deterministic truncation, bit-flip, and insertion cases. The helper never depends on wall-clock timing or ambient randomness, so an observed case can be reproduced from its seed.

These primitives are shared test infrastructure. Production code must continue to use its canonical transaction, persistence, process, scheduling, and verification authorities.

## Invariants

Resilience tests should fail closed and preserve these repository invariants when the relevant subsystem is exercised:

- terminal state cannot become active again;
- stale revisions, leases, process identities, receipts, or epochs do not regain authority;
- cancellation cannot later publish an authoritative success;
- malformed or corrupt input cannot fabricate accepted state;
- partial or failed persistence/integration cannot be reported as committed success;
- restart/replay reaches the same authoritative state allowed by the durable record;
- corrupt or truncated state is rejected, quarantined, or recovered according to the owning subsystem rather than guessed into validity.

A crash, panic on bounded malformed input, invariant violation, authority bypass, deadlock, or unrecoverable routine corruption discovered by this certification is a release blocker until fixed or explicitly dispositioned.

## Adding a resilience-sensitive boundary

When a new externally facing parser, durable state machine, identity/lease authority, or crash-sensitive commit boundary is introduced, extend an existing canonical subsystem test or add a focused property/fault target and include it in this certification workflow. Prefer deterministic model/property tests and explicit injected fault points over timing-dependent flakes.

The journal durability tests remain the canonical journal recovery proof; resilience certification invokes them rather than duplicating their implementation.
