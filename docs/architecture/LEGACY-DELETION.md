# Architecture v2 Legacy Deletion Checklist

A migration slice is incomplete until its superseded v1 path is removed or has a dated, owned follow-up. Compatibility adapters are temporary migration tools, not permanent architecture.

## Required evidence before deletion

- [ ] V2 owner and versioned contract are indexed.
- [ ] All production entrypoints and consumers use the v2 authority.
- [ ] Persistent state has a tested migration, rollback, and corruption path.
- [ ] Behavioral, recovery, and cross-platform conformance tests pass.
- [ ] Capability and UI claims are generated from the new authority.
- [ ] Observability distinguishes migrated, legacy, failed, and interrupted states.
- [ ] Release and downgrade compatibility are documented.
- [ ] The legacy code has no callers, feature flags, workflows, docs, or state readers.
- [ ] The related expected-failure fixture has become an unexpected pass and is removed in the same change.
- [ ] `baseline.json`, this checklist, and the relevant ADR are updated together.

## Program deletion targets

| Phase | Replacement authority | Required deletion target |
|---|---|---|
| #647 foundation | versioned IDs, errors, time, command/event/evidence contracts | duplicated primitives and incompatible envelopes |
| #648 orchestration | one session/plan/task/worker/review/verification core | parallel scheduler, task, worker, review, mutation, and verification authorities |
| #649 capabilities | generated registry plus certified dispatcher/permissions | advertised but unreachable tools and structural-only production claims |
| #650 provider/OAuth | durable route health, readiness, auth, and exact capabilities | per-surface readiness and process-local fallback authority |
| #651 frontends | shared command/event/evidence/artifact contracts | frontend-owned execution, provider, or completion semantics |
| #652 migration | versioned state migrations and deletion receipts | all superseded v1 code, state, docs, flags, workflows, and adapters |
| #653 unsafe/FFI | audited crate-local unsafe boundary | unscoped unsafe code and undocumented dynamic FFI |
| #655 updater | Ed25519-verified prebuilt artifact channel | default source compilation update path |

## Known-defect deletion targets

- [ ] #631: remove browser tool advertisement without dispatch, or replace it with certified dispatch and permission evidence.
- [ ] #632: remove integration-before-review ordering and any recovery path that assumes it.
- [ ] #633: remove verification APIs and receipts that omit changed paths.
- [ ] #634: remove decorative task, worker, reviewer, or verifier projections without durable execution evidence.
- [x] #636: remove provider capability, cancellation, fallback, and readiness paths that disagree with wire behavior.

### Parent-review contract deletion receipt (#632, partial)

- `medusa-review-model` owns a versioned parent-review response schema and fail-closed parser.
- The durable mutation transaction accepts only a final JSON envelope with `schema_version`, typed decision, and non-empty rationale.
- Free-form `MEDUSA_REVIEW_ACCEPTED` and `MEDUSA_REVISION_REQUESTED` markers no longer authorize integration.
- Unknown fields, malformed JSON, unsupported schemas, trailing text, and empty rationales fail closed.
- Remaining #632 work: replace the generic `AgentEngine` review transport with a dedicated no-tools reviewer while preserving durable session evidence.

### Provider deletion receipt (#636)

- Production adapters report wire-truthful capabilities and use abortable cancellable requests.
- `ProviderManager` persists route attempts, retries, failover, success, cache, and execution state in the active user-profile authority.
- The production `medusa-external-provider` facade constructs its manager with that durable authority and derives readiness from the same route health.
- The process-local `live_verified` readiness flag and in-memory production-manager construction path are deleted.
- Unit, architecture, cross-platform, package, safety, acceptance, and live-provider gates are required before merge.

## Final v1 removal gate

Phase #652 may close only when repository search and CI prove there are no unindexed legacy authorities, compatibility flags, stale architecture links, unknown components, or production capability claims without a certified dispatcher.
