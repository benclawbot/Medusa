---
name: implementation-workflow
description: Implement code changes end to end in any project. Use for features, fixes, refactors, migrations, tests, or repo edits where the agent must inspect, change, validate, document, and summarize work.
---

# Implementation Workflow

Follow this for real code changes in any language or framework.

## Phase 1 — Inspect

1. Read the task carefully.
2. Inspect the repository structure.
3. Read the relevant files before editing.
4. Identify existing conventions, test commands, build commands, and documentation locations.
5. State the likely change plan internally before touching files.

## Phase 2 — Edit narrowly

- Make the smallest coherent change that satisfies the task.
- Preserve existing architecture unless the task requires architecture change.
- Prefer existing libraries, patterns, naming, and test style.
- Do not add dependencies unless necessary and justified.
- Do not change public APIs, data formats, or behavior outside scope without calling it out.

## Phase 3 — Test while working

Run the narrowest useful check first, then broader checks.

Typical order:

```bash
# targeted tests first
<targeted test command>

# static checks
<typecheck/lint/format command>

# broad tests/build
<full test/build command>
```

If a check fails, stop feature work and switch to `debugging-root-cause`.

## Phase 4 — Documentation

Update docs when behavior, setup, commands, architecture, usage, or public surface changes. Prefer focused edits over dumping a new doc.

Check for:

- README usage/setup changes
- architecture notes
- API docs
- changelog/release notes
- examples
- migration notes

## Phase 5 — Final review

Before reporting success:

1. Inspect the diff.
2. Confirm no secrets or local-only paths were added.
3. Confirm tests actually cover the changed behavior.
4. Confirm docs match the final code.
5. Prepare a rollback path.

## Required final report

Always include:

- What changed and why
- What alternatives were rejected
- Tests run and exact output
- Risk level / blast radius
- Files touched and why each matters
- Known uncertainty
- Rollback plan
