# Medusa Architecture v2 Living Index

This is the change-governance root for Medusa architecture v2. It distinguishes **legacy availability** from **v2 certification**. Production code and executable tests remain the authority for what v1 currently does; [`baseline.json`](baseline.json) is the authority for migration ownership, certification status, state ownership, trust boundaries, and deletion targets.

## Phase 0 feature freeze

Major feature expansion is frozen while issues #646–#652 rebuild the core. Exceptions are limited to security or data-loss corrections, work required to enable architecture v2, the unsafe/FFI boundary in #653, and the verified prebuilt updater in #655. A release must not advertise a major capability as v2 production unless its owner, versioned contract, dispatcher, permissions, conformance evidence, migration consumer, and legacy deletion target are recorded in `baseline.json`.

## How to use this index

1. Find the capability or deployment mode in `baseline.json`.
2. Follow its dispatcher and implementation path to the real production entrypoint.
3. Find the related source-of-truth row, state machine, trust boundary, and known-failure fixture.
4. Record any authority, contract, dependency, persistence, lifecycle, entrypoint, capability, or trust-boundary change in this index and an ADR before merge.
5. Remove the corresponding legacy path only after migration consumers and rollback evidence are complete.

## Current v1 map

```mermaid
flowchart LR
  UI[TUI / CLI / Desktop / Daemon] --> R[RuntimeController]
  R --> P[Production orchestrator]
  P --> RO[Read-only planner and risk reviewer]
  P --> MW[Mutating worktree coordinator]
  MW --> WV[Worktree verification]
  WV --> I[Primary-tree integration]
  I --> PR[Parent read-only review]
  PR --> RV[Repository verification]
  R --> J[(Session journal and .medusa records)]
  RO --> J
  MW --> J
  RV --> J
```

This is an inventory, not the desired architecture. Known defects include integration before independent review, verification that does not receive changed paths, advertised browser tools without production dispatch, and provider capability claims that do not match wire or cancellation behavior.

## Target v2 map

```mermaid
flowchart LR
  S[Versioned frontend commands] --> O[Single orchestration core]
  O --> PA[(Plan aggregate)]
  PA --> W[Leased isolated worker]
  W --> V[Changed-path-aware verification]
  V --> R[Independent prepared-change review]
  R -->|accepted receipt| I[Single mutation and integration service]
  I --> PV[Primary repository verification]
  PV --> E[(Versioned evidence and artifact envelope)]
  O --> C[Generated capability registry]
  C --> D[Certified dispatchers and permission gates]
  O --> H[Durable provider route health]
```

V2 invariants:

- one authoritative owner for every mutable concern;
- no primary-tree integration before an accepted independent review receipt;
- changed paths remain explicit through implementation, verification, review, integration, and evidence;
- displayed tasks and workers require durable dispatch and lease evidence;
- an advertised capability requires a dispatcher, permission contract, tests, owner, observability, migration consumer, and deletion target;
- provider readiness and capability claims equal actual wire and cancellation behavior;
- frontends project commands, events, evidence, and artifacts but do not redefine execution semantics;
- failures and interrupted states remain evidence and are never rewritten as success.

## Deployment modes and shared path

| Mode | Entry point | Current implementation | Shared authority |
|---|---|---|---|
| Interactive terminal | `medusa` | `crates/medusa-tui` | `medusa-runtime::RuntimeController` |
| Headless | `medusa run` | `crates/medusa-cli` | `medusa-runtime::RuntimeController` |
| Daemon | `medusa daemon` | `crates/medusa-daemon` | `medusa-runtime::RuntimeController` |
| Desktop | `apps/medusa-desktop` | React/Tauri application | `medusa-runtime::RuntimeController` |
| GitHub operations | `medusa github` / capability entrypoints | `crates/medusa-github` | guarded typed operation contract |
| Update | `medusa update` | `crates/medusa-update` | legacy source-build path pending #655 |

## Capability certification

The legacy capability ledger remains in [`../CAPABILITY-CLAIMS.json`](../CAPABILITY-CLAIMS.json) and [`../CAPABILITY-EVIDENCE.md`](../CAPABILITY-EVIDENCE.md). Its `production` value means a current entrypoint exists; it does **not** mean architecture-v2 certification.

| Capability | Legacy claim | V2 status | Decision | Blocking evidence |
|---|---|---|---|---|
| Shared runtime | production | legacy-uncertified | replace authority contracts | lifecycle and ownership remain implicit |
| Durable sessions and memory | production | legacy-uncertified | adapt | projections must be separated from authority |
| GitHub service | production | legacy-uncertified | adapt | complete OAuth/backend contract migration |
| Provider/context resilience | production | quarantined | replace | streaming, cancellation, fallback-health mismatches |
| Identity, approvals, transactions | production | legacy-uncertified | adapt | centralize mutation receipts and authority |
| Daemon | production | legacy-uncertified | adapt | version daemon/remote contracts |
| Release trust | production | legacy-uncertified | adapt | connect #655 verified artifacts to updater |
| Self-update | production | quarantined | replace | source compilation is the default channel |
| Multi-agent research | production | quarantined | replace | review ordering, decorative state, changed-path loss |
| Browser tools | advertised | quarantined | replace | no production `execute_tool` dispatch |
| Plugins/extensions | structural | design-only | adapt | no certified manifest/permissions/lifecycle |
| Telegram remote frontend | partial | quarantined | adapt | shared-path and operator conformance incomplete |
| Unsafe/FFI boundary | partial | legacy-uncertified | adapt | #653 audit and allowlist |

## Source-of-truth matrix

The complete machine-readable matrix is in `baseline.json`. The critical rows are:

| Concern | Current authority | V2 authority | Non-negotiable invariant |
|---|---|---|---|
| Session/transcript | agent session journal | versioned session aggregate | presentations do not create history |
| Plan/task graph | session plan plus orchestrator contracts | one persisted plan aggregate | execution binds to the active fingerprint |
| Worker lifecycle | `WorkerExecutionController` state | one durable worker aggregate | visible workers require dispatch and lease proof |
| Mutation | worktree manager and transaction receipts | one mutation service | accepted review precedes integration |
| Review | parent review after integration | independent prepared-change review | integration requires an accepted receipt |
| Verification | repository gate and targeted checks | changed-path-aware receipt | changed paths survive every transition |
| Provider route/readiness | configuration plus process-local manager state | durable route health contract | claims equal wire behavior |
| Capability availability | legacy claims plus UI/docs | generated registry | no advertised action without certified dispatch |
| Evidence/artifacts | `.medusa` records and release evidence | versioned envelope | reports derive from evidence |
| Updates/releases | workflows plus source updater | Ed25519-verified prebuilt manifest | no silent source compilation |

## Dataflows

- **Session:** frontend command → runtime command envelope → session journal → durable projection → frontend event.
- **Execution:** plan aggregate → immutable task contract → lease → isolated implementation → changed-path verification → review receipt → integration receipt → repository verification.
- **Provider:** selected route → exact capability preflight → abortable request → response/usage event → durable route-health update.
- **Evidence:** command, worker, verification, review, integration, recovery, and artifact receipts → versioned evidence envelope → report/UI/release consumers.
- **Persistence:** every mutable concern identifies one journal or aggregate; caches and UI projections are reconstructable and never authoritative.

## Trust boundaries

The indexed boundaries are repository mutation, platform containment, unsafe/FFI, secrets, provider network, GitHub OAuth/API, browser sidecar, plugins, and release/update artifacts. #653 and #655 are phase-0 companions and remain allowed during the freeze because they close trust and distribution boundaries rather than expand product scope.

## Known-failure compatibility fixtures

The headless harness intentionally reproduces current defects as expected failures. They document what v1 does, not what v2 should preserve:

- `browser-dispatch-unreachable` (#631)
- `integration-precedes-parent-review` (#632)
- `isolated-verification-drops-changed-paths` (#633)
- `provider-capability-mismatch` (#636)

An unexpected pass fails the baseline job until the fixture, capability status, migration record, and deletion checklist are updated together.

## Extension procedure

A new crate, entrypoint, authority, capability, provider route, frontend, persistence record, trust boundary, or dependency edge requires:

1. a baseline entry with owner and preserve/adapt/replace/quarantine/delete disposition;
2. a versioned contract or an ADR explaining why no contract is needed;
3. dispatcher and least-privilege permission mapping;
4. black-box conformance and platform coverage;
5. observability and recovery semantics;
6. migration consumers and an explicit v1 deletion target;
7. architecture-impact declaration in the pull request.

## Migration and deletion

| Phase | Issues | Outcome |
|---|---|---|
| 0 | #646, #653, #655 | freeze, inventory, governance, unsafe boundary, verified updater |
| 1 | #647 | foundation contracts |
| 2 | #648 | one orchestration core and state ownership |
| 3 | #649 | capability registry, permissions, and dispatch |
| 4 | #650 | provider/OAuth route authority |
| 5 | #651 | all frontends on the shared core |
| 6 | #652 | state migration and v1 deletion |

Use [`LEGACY-DELETION.md`](LEGACY-DELETION.md) for deletion gates and [`RELEASE-POLICY.md`](RELEASE-POLICY.md) for freeze and release rules.

## Decisions, schemas, tests, and runbooks

- Decision: [`decisions/0001-architecture-v2-reset.md`](decisions/0001-architecture-v2-reset.md)
- Machine-readable baseline: [`baseline.json`](baseline.json)
- Legacy product map: [`../ARCHITECTURE.md`](../ARCHITECTURE.md)
- Contributor crate map: [`../CONTRIBUTOR-ARCHITECTURE.md`](../CONTRIBUTOR-ARCHITECTURE.md)
- Capability evidence: [`../CAPABILITY-EVIDENCE.md`](../CAPABILITY-EVIDENCE.md)
- Architecture checker: `python scripts/check-architecture-index.py`
- Adversarial checker fixtures: `python scripts/test-architecture-index.py`
- Headless compatibility harness: `python scripts/architecture-conformance.py --all --json`

The architecture baseline workflow runs these checks on Linux, macOS, and Windows.
