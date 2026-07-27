# Reproducible safety and recovery proof

This proof is a concise, public view of Medusa's production acceptance evidence. It intentionally delegates to `cargo product-acceptance`; it is not a parallel demo path.

## Run it

On a clean Linux checkout with Bubblewrap installed:

```bash
cargo medusa-proof --output medusa-proof-artifacts
```

The command requires no private provider credentials. It prints a terminal-friendly progress view and writes `medusa-proof-artifacts/medusa-proof.json` plus the underlying acceptance summary and scenario logs.

## Plan → Execute Safely → Recover

```text
┌──────────────┐     ┌──────────────────────┐     ┌────────────────────┐
│ Plan         │ ──▶ │ Execute Safely       │ ──▶ │ Recover            │
│ bounded task │     │ production runtime   │     │ resume / rollback  │
│ known repo   │     │ containment + policy │     │ verify final state │
└──────────────┘     └──────────────────────┘     └────────────────────┘
```

## Trust boundary

```text
                         outside trust boundary
             filesystem outside repo · network · stray processes
                                      ▲
                                      │ denied / terminated
                                      │
┌─────────────────────────────────────┴────────────────────────────────┐
│ Medusa production execution boundary                                │
│                                                                     │
│  plan + durable state ─▶ orchestrator ─▶ contained process tree     │
│                                  │                    │              │
│                                  ▼                    ▼              │
│                           verification gate     repository scope      │
│                                  │                    │              │
│                                  └──── evidence ──────┘              │
└─────────────────────────────────────────────────────────────────────┘
                                      │
                                      ▼
                         checkpoint / resume / rollback
```

## Evidence mapping

The proof fails if any required acceptance scenario disappears or fails. The machine-readable artifact maps each public guarantee to the exact acceptance scenario, command, duration, log, status, and failure detail.

| Public guarantee | Authoritative scenario |
| --- | --- |
| bounded coding task | `production-orchestration` |
| repository-bounded writes | `filesystem-network-process-boundary` |
| denied external filesystem action | `filesystem-network-process-boundary` |
| denied network action | `filesystem-network-process-boundary` |
| process-tree cleanup | `filesystem-network-process-boundary` |
| interruption and durable resume | `interruption-resume` |
| checkpoint restoration | `checkpoint-restore` |
| rollback after failed verification | `verification-rollback` |
| final repository verification | `headless-entrypoint` |
| corrupted-state recovery | `corrupted-state-recovery` |

The Linux-only public proof is deliberate: Linux currently has the production Bubblewrap evidence needed to exercise all containment claims together. macOS and Windows continue to run the supported product acceptance matrix, and the proof command refuses to claim guarantees that the selected platform did not exercise.

## Optional live-model layer

A live provider can be demonstrated separately, but it is not part of this deterministic proof and is not required for CI. Live-model output must not replace the acceptance evidence captured here.
