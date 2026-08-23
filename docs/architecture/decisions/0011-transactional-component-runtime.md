# ADR-0011: Transactional component runtime with explicit recovery boundaries

## Status

Accepted for the component-runtime contract and its incremental adoption.

## Date

2026-08-23

## Context

The component-runtime epic (#1036) requires one lifecycle authority for components that can be
activated, replaced, retired, and reconciled without leaving unowned resources or silently
changing dependency pointers. Issues #1037–#1049 define the required contract: stable identity
and generations, scoped host context, resource attribution, reversible effects, typed dependency
resolution, committed-versus-target views, provider withdrawal ordering, authoritative desired
state, revision compare-and-swap, validated self-modification, capability containment, external
commit semantics, and deterministic fault/invariant evidence.

These concerns cannot be represented safely by a collection of callbacks. A callback does not
prove which generation owns a resource, whether a dependency view was committed, whether a
failed inverse left cleanup debt, or whether a remote side effect has crossed its commit point.
The runtime therefore needs explicit records and typed transitions, while still allowing existing
product entrypoints to adopt the contract incrementally.

## Decision

`medusa-runtime::component_runtime` is the reference contract for transactional component
lifecycle. Every instance has a stable `ComponentId`, a monotonic `ComponentGeneration`, an
identity tuple, a scoped context, and provenance that binds it to a desired-state revision and
source. Lifecycle state is explicit; retiring or blocked instances cannot become active again.

Resources are registered against the exact `(component, generation)` owner. Exclusive resources
are claimed before activation and released only during a successful withdrawal. The
`EffectJournal` records an inverse only after its forward effect succeeds, rolls back in reverse
order, is idempotent, and records cleanup debt when an inverse fails. Process-death metadata stays
inspectable so recovery can continue after the owning process exits.

Dependencies are declared as typed `requires` and `provides` cards. Resolution is deterministic,
rejects undeclared or ambiguous providers, and detects cycles. A component keeps a committed view
and a separately computed target view; a changed target produces an explicit reconciliation action
rather than a silent pointer swap. Provider retirement first prevents new resolution, tears down
consumers using the committed provider, and withdraws the provider only after consumer and
provider cleanup succeed.

Desired state is authoritative and versioned. Mutations are validated before a single
compare-and-swap commit, persisted through a temporary file and rename, and may be retried with an
idempotency key. The self-modification facade accepts only typed proposals carrying a base
revision, source provenance, and an auditable preview. Runtime code, not an agent, owns
reconciliation and application.

Capability resolution produces one set that feeds host authority and platform containment intent.
Unsupported controls fail closed with a typed error. A generation-and-revision-bound policy
fingerprint makes stale capability decisions visible. Irreversible external operations are not
inserted into the reversible effect journal; `ExternalCommitLedger` records idempotency,
at-most-once versus at-least-once semantics, uncertain commit points, and compensation-required
states without inventing a fake inverse.

The contract exposes deterministic fault points and replay fingerprints. Runtime invariant
checks cover identity/context and journal ownership, capability drift, lifecycle/retirement
states, generation counters, exclusive-resource ownership, and dependency references. The
integration tests exercise activation failure, candidate-health rejection, cleanup blocking,
external uncertainty, and replayed fault traces.

### Lifecycle and recovery state machine

```text
Inactive -> Activating -> Active -> Deactivating -> Inactive
                      |       \-> Retiring -> Inactive (withdrawn)
                      v
                    Failed

Any deactivation/retirement inverse failure -> BlockedRetirement
BlockedRetirement remains inspectable and cannot be advertised as Active.
Replacement prepares a new generation, validates health and dependencies, migrates consumers,
then withdraws the old generation; any pre-commit failure removes the candidate and restores the
committed consumer view.
```

## Alternatives considered

### Global mutable component handles

Rejected because a global handle cannot attribute resources to a generation or distinguish a
committed dependency from a newly computed target.

### Pointer swaps with best-effort cleanup

Rejected because consumers can observe a provider that was never committed, and failed cleanup is
otherwise lost. Explicit journals and dependency views make debt and blocked retirement visible to
the caller.

### Treat every side effect as reversible

Rejected because external commits, such as a charge or publication, may have no safe inverse.
Those operations need idempotency and compensation semantics at their commit boundary.

### Agent-owned direct mutation

Rejected because proposals must be validated against the current revision and applied only by the
reconciler. This keeps provenance, stale-conflict handling, and policy checks in one authority.

## Consequences

The runtime gains a small, serializable vocabulary for lifecycle, ownership, dependency, policy,
and recovery evidence. Callers must explicitly handle typed conflicts, unsupported containment,
blocked cleanup, uncertain external commits, and stale proposals. The component-runtime module is
intentionally an adoption seam; it does not claim to provide live credentials, microphone/audio,
Telegram, or authenticated OpenAI Realtime evidence. Those remain external acceptance work tracked
by issue #719.

Focused conformance lives in `crates/medusa-runtime/tests/component_runtime_*.rs`. The tests are
the behavioral authority for this contract; broader workspace checks remain separate evidence and
must not be represented as green when they are interrupted or fail for unrelated baseline debt.
