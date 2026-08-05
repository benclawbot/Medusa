# Architecture v2 final certification audit

Issue: #654
Parent: #645

## Result

Certification is not complete until every migration-era production claim below is corrected and the architecture checker rejects its reintroduction.

## Observed deviations

1. Root `Cargo.toml` still describes the production mutation path as ending in a read-only parent `medusa-agent::AgentEngine`. Production now uses the dedicated zero-tool durable parent reviewer before integration, followed by independent verification, authorization, integration, reconciliation, and canonical terminal persistence.
2. `docs/architecture/production-multi-agent-consolidation.md` repeats the obsolete parent-AgentEngine path and assigns review authority to the generic agent session.
3. `docs/architecture/INDEX.md` still presents an active phase-0 feature freeze, a current-v1 map, migration-in-progress language, review-after-integration wording, and multiple `legacy-uncertified` statuses for paths already certified through production entrypoints.
4. `docs/architecture/baseline.json` still records the feature freeze as active, retains migration-era component dispositions and capability gaps, identifies a legacy post-integration parent reviewer, and contains an obsolete integrate-before-review state machine alongside the production v2 state machine.
5. The architecture checker validates schema and path consistency but does not currently fail when final-certification metadata reintroduces an active migration freeze, review-after-integration authority, or a production path ending in the generic parent agent.

## Required corrections

- Update root workspace metadata to name the actual production execution sequence and dedicated reviewer.
- Update the consolidation document to distinguish generic bounded model sessions from dedicated review authority.
- Convert the living index from migration-state language to certified-production language while retaining historical phase receipts.
- Convert the machine-readable baseline from an active migration manifest into a final certification record:
  - feature freeze inactive;
  - one production execution state machine;
  - review before integration;
  - dedicated parent reviewer authority;
  - certified production statuses for completed v2 authorities;
  - no obsolete duplicate authority or compatibility path.
- Extend `scripts/check-architecture-index.py` and adversarial fixtures so certification fails closed on those stale claims.
- Run architecture policy, architecture baseline on Linux/macOS/Windows, CI, product acceptance, safety/recovery, daemon, desktop packaging, updater, release gates, and both live provider entrypoint proofs.

## Closure rule

Issue #654 and parent #645 may close only after the correction PR is merged to `main`, every required workflow is green, the repository contains no selectable v1 review or mutation compatibility path, and this audit has no unresolved deviation.
