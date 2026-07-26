# medusa-testkit

Shared deterministic fixtures for Medusa tests.

## Guarantees

- Fixtures use stable identifiers and timestamps so replay and serialization assertions are reproducible.
- Every generated protocol envelope is expected to pass `EventEnvelope::validate`.
- The crate is test support only. Product crates must include it under `[dev-dependencies]`; production dependency edges are prohibited.
- A fixture belongs here only when it has multiple credible consumers. One-off setup remains local to its test suite.

## Current consumers

The agent, runtime, and daemon crates depend on this crate only for tests. Runtime and agent integration suites exercise deterministic protocol serialization and replay behavior.
