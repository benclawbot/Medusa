# Medusa — Production-Grade Autonomous CLI Coding Agent

**Document status:** Implementation contract provenance record  
**Version:** 1.1.0  
**Canonical source SHA-256:** `f6846b8570cacff1b4f4766e2fc564b60646fd3e1a18301e8edafe5a4d8d7dab`

The authoritative 3,445-line specification supplied for this implementation governs this repository. Phase 0 implements section 31's repository-and-contracts milestone and stops at its human review gate.

## Phase 0 contract

Deliver:

- Cargo workspace;
- protocol types;
- event schemas;
- configuration;
- errors;
- testkit;
- CI;
- formatting and lint policy.

Exit criteria:

- all schema round-trip tests pass;
- compatibility versioning is established;
- the implementation stops for explicit human review before Phase 1.

## Required phase report

Every gate reports what changed and why, rejected alternatives, exact tests and output, risk and blast radius, files touched, known uncertainty, and rollback instructions.

> Repository transport note: the connected GitHub API cannot ingest a local attachment by file reference. This provenance record pins the exact authoritative source hash; the complete source remains the user-supplied `MEDUSA_SPEC-1.md` used during implementation.
