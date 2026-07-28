---
name: doctor
description: Diagnose Medusa, repository, toolchain, and agent-workflow health; identify recurring failure patterns; and apply safe, evidence-backed improvements.
---

# Doctor

Run a structured health check for the current Medusa workspace and improve problems that can be fixed safely.

This skill is inspired by [`claude-doctor`](https://github.com/millionco/claude-doctor), especially its detection of edit thrashing, repeated tool failures, excessive exploration, repeated instructions, correction-heavy sessions, and agent drift. Do not require Claude Code or `claude-doctor` to be installed: use them when available, but always provide a useful native Medusa diagnosis.

## Invocation

- `/doctor` — run the complete health check and propose or apply safe improvements.
- `/doctor quick` — run only high-signal, low-cost checks.
- `/doctor deep` — include broader tests, recent-session pattern analysis, and configuration review.
- `/doctor report` — diagnose without modifying files.
- `/doctor fix` — apply deterministic, low-risk fixes after showing the evidence.

Treat an omitted mode as `quick` first. Escalate to deeper checks only when the quick pass finds a problem or the user asks for a complete audit.

## Principles

1. Diagnose before changing anything.
2. Collect failures in batches rather than fixing one symptom at a time.
3. Prefer repository-local, deterministic checks.
4. Never expose API keys, tokens, credential files, private prompts, or transcript content.
5. Do not weaken tests, security controls, sandboxing, permissions, or lint rules to make checks pass.
6. Do not edit generated files unless the repository explicitly requires it.
7. Keep changes focused and reversible.
8. Re-run the exact failing checks after each fix, then run the relevant broader verification.
9. Separate observed facts from hypotheses.
10. Stop repeating the same failed approach after two materially identical failures.

## Phase 1: Establish context

Determine and report:

- repository root and current branch;
- whether the worktree is clean;
- active pull request, if discoverable;
- operating system and architecture;
- available Rust, Cargo, Git, Node.js, npm/npx, and GitHub CLI versions;
- relevant Medusa configuration locations;
- whether another active branch or pull request appears to touch the same files.

Do not modify or discard existing uncommitted user changes.

## Phase 2: Native Medusa health checks

Run the cheapest checks first and retain all failures before editing:

1. `cargo metadata --no-deps --format-version 1`
2. `cargo fmt --all -- --check`
3. focused `cargo check` for crates implicated by current changes, otherwise `cargo check --workspace --all-targets`
4. focused tests for changed crates, otherwise the smallest meaningful workspace test set
5. `cargo clippy --workspace --all-targets --all-features -- -D warnings` when time and environment permit
6. repository policy, guardrail, or validation scripts documented by the project
7. Git status, merge-conflict markers, unexpectedly large files, and obvious generated-artifact drift

In `quick` mode, stop after metadata, formatting, focused check, focused tests, and repository guardrails unless a failure requires expansion.

If a command cannot run because of an environmental limitation, classify it as `blocked`, explain the exact dependency, and continue with checks that remain possible.

## Phase 3: Agent-workflow diagnosis

When session or activity evidence is available, look for these patterns without printing sensitive transcript content:

- **edit thrashing** — the same file is repeatedly changed instead of being understood and updated coherently;
- **error loop** — three or more consecutive failures without a meaningful strategy change;
- **excessive exploration** — extensive reading or searching with little implementation progress;
- **restart cluster** — repeated fresh attempts at the same task;
- **correction-heavy interaction** — the user repeatedly has to redirect or restate requirements;
- **keep-going loop** — progress is split into unnecessarily tiny increments that require repeated user prompts;
- **repeated instructions** — constraints already provided by the user are forgotten or re-asked;
- **negative drift** — later work moves away from the original request;
- **validation churn** — many narrow validation cycles are run when one larger coherent change and batched verification would be more efficient;
- **overlapping work** — another agent or pull request is modifying the same area.

Use aggregate counts, filenames, command names, timestamps, and failure categories. Never quote private conversation text unless the user explicitly asks.

### Optional claude-doctor integration

If `claude-doctor` is already available, or `npx` can run it without changing repository dependencies, it may be used as supplemental evidence:

```bash
claude-doctor --json
claude-doctor --rules
```

Do not install it globally without explicit user approval. Do not treat its output as authoritative; validate recommendations against Medusa's repository rules and current architecture.

## Phase 4: Produce a health report

Present a compact report with these sections:

### Overall status

One of: `healthy`, `degraded`, `unhealthy`, or `blocked`.

### Findings

For every finding include:

- severity: critical, high, medium, low, or informational;
- evidence: command, file, or aggregate activity signal;
- impact;
- recommended action;
- whether it is safe to fix automatically.

### Verification matrix

Show each check as passed, failed, or blocked. Do not claim a check passed unless it actually ran successfully.

### Improvement plan

Order work by:

1. security or data-loss risk;
2. broken builds and tests;
3. correctness and reliability;
4. repeated workflow inefficiency;
5. polish and maintainability.

## Phase 5: Improve safely

In `report` mode, stop after the report.

In normal or `fix` mode, apply deterministic low-risk improvements when supported by evidence, including:

- formatting fixes;
- localized compiler, lint, and test fixes;
- stale or contradictory repository guidance;
- missing targeted tests for a confirmed regression;
- clearer validation commands;
- narrowly scoped AGENTS.md or Medusa skill guidance that prevents a demonstrated recurring failure pattern;
- consolidation of repeated tiny edits into one coherent change;
- removal of dead or misleading diagnostics.

Before editing AGENTS.md, CLAUDE.md, or another persistent instruction file:

1. show the observed recurring pattern;
2. verify that an equivalent rule does not already exist;
3. write the narrowest actionable rule;
4. avoid tool-specific rules when a general engineering rule is sufficient.

Do not automatically:

- install global packages;
- change user-level shell configuration;
- modify credentials or authentication;
- delete caches or user data;
- rewrite broad architecture;
- merge, release, or publish;
- change branch protection or CI policy;
- suppress warnings or skip tests.

## Phase 6: Verify and summarize

After changes:

1. re-run every check that previously failed;
2. run focused tests for modified crates or files;
3. run the broadest practical repository validation once, not repeatedly after every small edit;
4. inspect the final diff for unrelated changes, secrets, debug output, and generated noise;
5. report remaining failures and blocked checks honestly.

Finish with:

- health status before and after;
- files changed;
- fixes applied;
- checks passed, failed, or blocked;
- remaining recommended work;
- whether the workspace is ready for review.
