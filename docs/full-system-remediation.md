# Full-System Remediation

> Historical record — retained as implementation evidence; it is not current setup or status guidance. Start at [the documentation index](README.md).

This document records implementation-backed repairs discovered during the repository-wide audit.

## Phase 1

- Removed obsolete source duplicates that were outside the active module graph.
- Connected `agent.parallel_workers` to the live read-only tool scheduler.
- Preserved the scheduler's hard concurrency ceiling of eight.
- Added crate-level regression coverage for configured concurrency limits.

The public README will be updated after all selected remediation phases are implemented and validated.
