# Architecture v2 Release and Feature-Freeze Policy

## Freeze

Major feature expansion is frozen from phase 0 until the architecture program explicitly lifts the freeze. Allowed exceptions are:

- security or data-loss corrections;
- work required to enable or validate architecture v2;
- #653 unsafe/FFI boundary enforcement;
- #655 free Ed25519-verified prebuilt update delivery.

A change is not an exception merely because it can be implemented inside an existing crate.

## Capability claims

Legacy `production` in `docs/CAPABILITY-CLAIMS.json` means a current supported entrypoint exists. It is not architecture-v2 certification.

A capability may be advertised as v2 `certified-production` only when all of the following are present and indexed:

- one owner and one authoritative lifecycle;
- a versioned contract;
- a reachable production dispatcher;
- explicit least-privilege permissions and trust boundaries;
- behavioral and black-box conformance evidence;
- supported-platform declarations and required prerequisites;
- observability and durable failure/recovery evidence;
- migration consumers and an explicit legacy deletion target.

Known gaps must be labelled `legacy-uncertified`, `quarantined`, or `design-only`. Structural code is not a shipped capability.

## Release gate

A release candidate must pass:

1. the architecture index and adversarial checker tests;
2. the headless compatibility harness;
3. Linux, macOS, and Windows architecture baseline jobs;
4. existing security, dependency, daemon, desktop, release, and workflow guardrails;
5. capability evidence validation;
6. migration and deletion checks for any changed authority.

An expected-failure compatibility fixture is acceptable only while it is explicitly linked to an open repair issue, marked `desired=false`, and accompanied by a removal condition. An unexpected pass blocks the baseline until the capability status, fixture, migration record, and deletion checklist are updated together.

## Distribution

The v2 update channel must consume immutable prebuilt artifacts verified by a repository-owned Ed25519 manifest as defined by #655. It must not require paid platform signing and must not silently fall back to source compilation. Platform signing may remain an optional distribution enhancement, not the trust root.

## Lifting the freeze

The freeze may be lifted only by an ADR after:

- all production entrypoints use the shared v2 contracts;
- quarantined capability claims are repaired, downgraded permanently, or deleted;
- no duplicate mutable authority remains in the source-of-truth matrix;
- required v1 deletion targets are complete;
- cross-platform conformance and release gates pass.
