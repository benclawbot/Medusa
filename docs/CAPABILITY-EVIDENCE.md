# Medusa Capability Evidence

Status snapshot: **July 18, 2026**, based on `main` through merged PR #38. This document is an evidence ledger, not a promise that every long-term product goal is complete.

## Evidence rules

A capability is listed as **shipped** only when its production code is on `main` and covered by the repository's normal validation. Open pull requests, temporary writer workflows, branch-only diagnostics, and design intentions are listed separately and do not count as shipped.

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
| Markdown rendering and mid-turn follow-up queueing | `crates/medusa-tui`; merged in PR #34 | Workspace tests and source-size guardrail |
| Clipboard text and screenshot prompts | `crates/medusa-tui` | Workspace tests and cross-platform package smoke |
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

## Active work, not yet shipped

PR #39, `refactor: extract frontend-neutral runtime`, is moving interactive session control from the TUI into a reusable `medusa-runtime` crate. Its purpose is to let the terminal interface and the planned Zeus-derived desktop interface share one application core. Until that pull request merges and passes canonical validation, the runtime extraction and desktop entry point remain **in progress**.

## Documentation policy

`README.md` is the product overview and installation guide. This file is the status/evidence ledger. Historical phase documents may explain intent, but they must not claim completion that is contradicted by `main`, open pull requests, or required checks. A new completion snapshot should update this ledger instead of creating another competing `FINAL.md`.
