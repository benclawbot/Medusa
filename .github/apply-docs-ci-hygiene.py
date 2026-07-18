from pathlib import Path

root = Path('.')
readme_path = root / 'README.md'
readme = readme_path.read_text()

status_section = '''## Current status and evidence

The original phase labels are historical planning shorthand, not the current source of truth. As of July 18, 2026, `main` includes the work merged through PR #38: the Rust agent core, interactive TUI, durable sessions and memory, guarded repository tools, browser verification, parallel workers, release hardening, Markdown rendering, mid-turn follow-ups, and the optional Desktop Commander MCP integration.

| Area | Current evidence |
|---|---|
| Interactive product surface | `medusa` launches the TUI; transcript preservation, Markdown rendering, clipboard input, cancellation, usage metrics, skills, and queued follow-ups are implemented in `medusa-tui`. |
| Agent and repository runtime | Session orchestration, planning, tools, policy, verification, and persistence are implemented across `medusa-agent`, `medusa-intelligence`, `medusa-memory`, and related crates. |
| Extensions and MCP | Skills, hooks, MCP isolation, and the pinned Desktop Commander adapter are implemented in `medusa-extensions` and documented below. |
| Release evidence | `CI`, `Refactor Guardrails`, and `Release Gates` enforce formatting, Clippy, workspace tests, documentation, dependency policy, source-size limits, coverage, adversarial tests, package smoke tests, and live-provider scenarios. |
| Active architecture work | PR #39 is extracting a frontend-neutral `medusa-runtime` before the Zeus-derived desktop interface is wired to the same core. This work is not claimed as shipped until it merges and its canonical gates pass. |

See [Capability evidence](docs/CAPABILITY-EVIDENCE.md) for the auditable mapping from shipped capabilities to code and gates. Historical completion summaries should not override the current repository, merged pull requests, or required checks.

'''

if '## Current status and evidence' not in readme:
    marker = '## Requirements\n'
    assert marker in readme, 'README requirements marker not found'
    readme = readme.replace(marker, status_section + marker, 1)

security_link = '- [Security hardening](docs/SECURITY-HARDENING.md)\n'
evidence_link = '- [Capability evidence](docs/CAPABILITY-EVIDENCE.md)\n'
assert security_link in readme, 'README documentation list marker not found'
if evidence_link not in readme:
    readme = readme.replace(security_link, security_link + evidence_link, 1)
readme_path.write_text(readme)

release_path = root / '.github/workflows/release-gates.yml'
release = release_path.read_text()
release = release.replace(
    'on:\n  pull_request:\n  workflow_dispatch:\n',
    'on:\n  pull_request:\n    types: [opened, synchronize, reopened, ready_for_review]\n  workflow_dispatch:\n',
    1,
)
condition = "    if: ${{ github.event_name == 'workflow_dispatch' || github.event.pull_request.draft == false }}\n"
for job in [
    'coverage',
    'adversarial-regression',
    'fuzz-smoke',
    'chaos-and-migrations',
    'package-smoke',
    'security',
    'docs-and-schema',
    'live-minimax-coding-e2e',
]:
    marker = f'  {job}:\n'
    assert marker in release, f'release job marker missing: {job}'
    replacement = marker + condition
    if replacement not in release:
        release = release.replace(marker, replacement, 1)
release_path.write_text(release)

refactor_path = root / '.github/workflows/refactor-guardrails.yml'
refactor = refactor_path.read_text()
old_trigger = '''on:
  pull_request:
  push:
    branches: [main]
'''
new_trigger = '''on:
  pull_request:
    paths:
      - 'Cargo.toml'
      - 'crates/**'
      - 'scripts/check-source-size.sh'
      - 'docs/REFACTOR-BASELINE.md'
      - 'docs/PUBLIC-API-BASELINE.md'
      - 'docs/BENCHMARKS.md'
      - 'docs/source-size-exceptions.txt'
      - '.github/workflows/refactor-guardrails.yml'
  push:
    branches: [main]
    paths:
      - 'Cargo.toml'
      - 'crates/**'
      - 'scripts/check-source-size.sh'
      - 'docs/REFACTOR-BASELINE.md'
      - 'docs/PUBLIC-API-BASELINE.md'
      - 'docs/BENCHMARKS.md'
      - 'docs/source-size-exceptions.txt'
      - '.github/workflows/refactor-guardrails.yml'
'''
assert old_trigger in refactor, 'refactor guardrail trigger block not found'
refactor_path.write_text(refactor.replace(old_trigger, new_trigger, 1))

evidence = '''# Medusa Capability Evidence

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
'''
(root / 'docs/CAPABILITY-EVIDENCE.md').write_text(evidence)

for stale in [
    '.github/workflows/apply-desktop-commander-mcp.yml',
    '.github/workflows/wire-desktop-commander-mcp.yml',
    '.github/workflows/finalize-desktop-commander.yml',
    '.github/workflows/fix-tui-question-transcript-test.yml',
    '.github/workflows/remove-dead-system-prompt-wrapper.yml',
    '.github/workflows/extract-runtime-writer.yml',
]:
    assert not (root / stale).exists(), f'stale one-off workflow still present: {stale}'
