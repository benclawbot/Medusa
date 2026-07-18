# Medusa Capability Evidence

Status snapshot: **July 18, 2026**, based on `main` through merged PR #39. This document is an evidence ledger, not a promise that every long-term product goal is complete.

## Evidence rules

A capability is listed as **shipped** only when its production code is on `main` and covered by the repository's normal validation. Open pull requests, temporary writer workflows, branch-only diagnostics, and design intentions are listed separately and do not count as shipped.

When a production boundary moves, its behavior tests must move with it or be replaced at the correct layer; orphaned test files and coverage percentages alone are not completion evidence.

The authoritative order is:

1. production code and tests on `main`;
2. required GitHub Actions gates;
3. merged pull-request history;
4. this evidence summary;
5. historical phase plans.

## Shipped on `main`

| Capability | Production evidence | Gate evidence |
|---|---|---|
| CLI and interactive entry point | `crates/medusa-cli`, `crates/medusa-tui` | Workspace build, Clippy, tests, docs, and package smoke jobs |
| Full conversation transcript and distinct user/assistant presentation | `crates/medusa-tui` | TUI tests in the workspace suite; merged before PR #34 |
| Markdown rendering and mid-turn follow-up queueing | `crates/medusa-runtime`, `crates/medusa-tui`; introduced in PR #34 and moved behind the shared runtime in PR #39 | Workspace tests and source-size guardrail |
| Clipboard text and screenshot prompts | Frontend-neutral prompt types in `crates/medusa-runtime`; OS clipboard access in `crates/medusa-tui` | Runtime/TUI tests and cross-platform package smoke |
| Shared frontend-neutral interactive runtime | `crates/medusa-runtime`; extracted in PR #39, with `crates/medusa-tui` reduced to a terminal adapter | Runtime behavior tests, TUI mapping tests, workspace Clippy/tests, coverage, and package smoke |
| Agent loop, planning, cancellation, tools, and verification | `crates/medusa-agent`, `crates/medusa-protocol`, `crates/medusa-provider` | Workspace tests plus named adversarial regressions |
| Repository parsing, patching, and guarded transactions | `crates/medusa-intelligence`, `crates/medusa-agent` | Patch-transaction regression and workspace tests |
| Durable Markdown memory and lifecycle controls | `crates/medusa-memory` | Workspace tests and migration/rollback checks |
| Parallel workers and deterministic merge handling | `crates/medusa-workers` | Parallel merge and conflict-abort regressions |
| Browser verification sidecar | `crates/medusa-browser`, `crates/medusa-browserd` | Workspace tests and package validation |
| Skills, hooks, and MCP isolation | `crates/medusa-extensions` | Malicious-MCP regression and workspace tests |
| Desktop Commander MCP integration | `crates/medusa-extensions`; merged in PR #37 with lockfile follow-up in PR #38 | MCP tests, dependency policy, and canonical CI |
| Operational hardening, migrations, archives, redaction, and recovery | `crates/medusa-hardening` | Release Gates: coverage, adversarial regressions, fuzz, chaos, security, and package smoke |

## Canonical gates

- **CI** runs the complete workspace quality suite and dependency policy. Its concurrency group cancels superseded runs for the same ref.
- **Refactor Guardrails** enforces the 800-line production source ceiling and baseline documents. It is path-filtered to relevant code and baseline changes.
- **Release Gates** runs the expensive coverage, adversarial, fuzz, chaos, cross-platform packaging, documentation/schema, and live-provider jobs. Draft pull requests skip these expensive jobs; marking a pull request ready for review activates them, and later non-draft pushes re-run them.

Skipping expensive release jobs on drafts changes scheduling, not acceptance criteria: merge readiness still requires the full configured gate set.

## Next architecture work

The frontend-neutral runtime extraction is shipped in PR #39. The Zeus-derived desktop interface is still **not shipped**; it must consume `medusa-runtime` rather than duplicate session, provider, cancellation, follow-up, or event logic, and it requires its own canonical validation before the evidence ledger can claim a desktop entry point.

## Documentation policy

`README.md` is the product overview and installation guide. This file is the status/evidence ledger. Historical phase documents may explain intent, but they must not claim completion that is contradicted by `main`, open pull requests, or required checks. A new completion snapshot should update this ledger instead of creating another competing `FINAL.md`.
