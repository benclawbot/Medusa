# ADR-0010: Typed non-authority service-provider composition

## Status

Accepted for the provider seam; production adoption remains incremental.

## Date

2026-08-21

## Context

Several runtime capabilities can safely vary by implementation: repository search, code
intelligence, web access, artifact storage, terminal adapters, subordinate protocol clients, and
diagnostic exporters. The implementation boundary was not previously uniform. A free-form
`service_provider` setting could therefore describe a capability without declaring its lifecycle,
authority, cancellation behavior, or evidence identity.

The runtime authorities remain fixed. In particular, a service provider must not replace policy,
approval, containment, mutation provenance, verification, integration review, the durable journal,
or protected improvement evaluation.

## Decision

`medusa-runtime::service_provider` defines a versioned `ServiceProvider` trait and a
`ServiceProviderRegistry`. Providers declare stable service/provider identities, versions,
capability class, secret-free configuration fingerprint, input/output schemas, required authority,
runtime-owned boundaries, concurrency, cancellation, and truthful health. The registry validates
those declarations, rejects fixed-authority plugins, binds leases to an explicit generation, and
owns start/stop plus deterministic unregister behavior.

Execution receives a runtime-owned cancellation flag and a generation-bound request. A stale
generation or cancelled request is rejected before provider code runs. Provider responses carry
the declared identity, generation, schema version, and a deterministic evidence fingerprint; the
registry validates those fields before returning the response to a caller.

Runtime configuration keeps its existing serialized field for compatibility, but the new
`compile_effective_config_with_registry` entry point admits a selected provider only when an
explicit registry contains that provider. The legacy compiler has no registry and continues to
fail closed for any selected service provider. This prevents configuration text from silently
constructing a service or weakening an authority boundary.

The canonical behavioral-health contract is also re-exported from `medusa-runtime`, so CLI,
daemon, TUI, and embedded callers can consume the same versioned snapshot type rather than
reimplementing status semantics.

## Alternatives considered

### Free-form provider names

Rejected because a string does not prove readiness, lifecycle ownership, cancellation semantics,
generation compatibility, or authority boundaries.

### Dynamic authority plugins

Rejected because replacing root orchestration, policy, approval, containment, mutation,
verification, integration, journaling, or improvement authority would make the certified tool
pipeline unverifiable.

### One global provider implementation

Rejected because it preserves hidden coupling and prevents safe substitution for independent
capability families. The seam is intentionally generic, while adoption can proceed family by
family.

## Consequences

Providers have a concrete conformance contract and durable identity that can be attributed in
future evidence and telemetry. Generation and lease checks make restart and configuration drift
fail closed, while explicit boundaries keep service implementations subordinate to Medusa-owned
authorities. The registry does not by itself complete migration of every built-in tool family;
those production integrations and live acceptance evidence remain separate work.
