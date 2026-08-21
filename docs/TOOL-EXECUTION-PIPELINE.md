# Certified tool execution pipeline

Medusa model-executable tools use one versioned execution lifecycle. The pipeline is an extension seam, not a replacement for Medusa's existing authorities.

## Required order

Every certified invocation traverses these stages exactly once, in order:

1. `resolve` — snapshot capability and production handler identity.
2. `pre_execute` — validate/canonicalize input; transforms may only narrow authority.
3. `guards` — evaluate capability readiness and monotonic security guards.
4. `approval` — resolve approval only after guards; approval cannot restore a denial.
5. `around_dispatch` — cancellation/deadline/resource/telemetry wrappers without authority changes.
6. `execute` — call the existing production handler/containment/mutation path.
7. `post_execute` — normalize/redact/quarantine output without turning failure into success.
8. `finalize` — freeze one typed terminal outcome and authority receipts.
9. `publish` — durably journal the authoritative result before model/frontend projection.

Malformed, skipped, repeated, or reordered required stages fail closed. Once a guard denies a call, all later guard decisions remain denied. Approved retries re-enter the certified pipeline and re-evaluate guards.

`finalize` produces one `CanonicalToolResultV1` with a stable result fingerprint. Consumers receive
explicit projections: complete machine/Code Mode data, bounded and optionally redacted model data,
audit-safe durable evidence, or frontend presentation. Projection metadata records the schema,
original/projected size, omission reason, redaction, and expansion availability; a model-visible
artifact handle is only a reference and must be revalidated against the repository's
`.medusa/artifacts` boundary before reading.

Shell output that exceeds the model budget is spilled as a redacted, content-addressed artifact;
the canonical result records that safe reference while the model receives only the bounded projection.

## Fixed authorities

Middleware may attach only at certified seams. It must not replace or weaken:

- `RuntimeController` and production orchestration;
- `CapabilityRegistry` readiness/projection authority;
- `AgentExecutionPolicy`, task/write-scope, and hard-denial policy;
- approval/grant authority;
- process/browser/executable-skill containment;
- mutation provenance, review, and integration authorities;
- independent verification/evidence authority;
- durable session journal authority.

The design rule is: **extensibility attaches to certified seams; authorities are not plugins.**

## Production composition

`crates/medusa-agent/src/tool_pipeline.rs` defines the versioned typed lifecycle and immutable terminal outcome. `crates/medusa-agent/src/tools/mod.rs` composes capability readiness and `AgentExecutionPolicy` into ordered monotonic guards, then invokes existing tool handlers. `crates/medusa-agent/src/engine.rs` passes the active execution policy through normal calls, early streamed read-only dispatch, approval/retry, mutation-provenance execution, and parallel tool-DAG dispatch.

Specialized handlers keep their existing ownership. Browser calls still use the verified sidecar/session policy, executable skills retain containment, compound tools retain their internal authorities, and filesystem mutation remains on the transaction/provenance path.

## Invariants for new executable handlers

A new model-executable handler is not production-valid merely because it is registered or callable. It must:

- resolve to a truthful capability/handler identity;
- enter the certified lifecycle before handler execution;
- inherit all active monotonic guards;
- preserve cancellation and structured errors;
- produce one immutable final outcome per invocation;
- publish the authoritative result durably before presentation/model projection;
- declare lifecycle ownership so unload/disposal cannot leave middleware active.

A subordinate execution mechanism is permitted only when the parent certified invocation records and verifies the child identity/authority chain and the child cannot bypass parent guards.

## Conformance

Repository CI must cover normal, early-stream, approved, browser, executable-skill, compound, and parallel/DAG paths. Cross-platform production entrypoints must preserve the same ordering and fail-closed behavior on Linux, macOS, and Windows.
