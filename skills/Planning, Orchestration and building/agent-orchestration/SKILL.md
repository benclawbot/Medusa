---
name: agent-orchestration
description: Plan and coordinate multi-step coding-agent work. Use when a task needs planning, delegation, checkpoints, autonomous loops, stop conditions, or coordination across tools/agents. Generic: does not assume any specific agent product.
---

# Agent Orchestration

Use this skill when the work is too large for a single direct response or requires multiple phases, tools, or agent sessions.

## Core contract

Every orchestrated task must define:

1. **Objective** — one concrete outcome.
2. **Scope** — what files, areas, or behavior may change.
3. **Constraints** — what must not change.
4. **Validation** — exact commands, checks, or evidence that prove progress.
5. **Stop condition** — when to finish, pause, or ask for human input.
6. **Documentation expectation** — what docs, README sections, comments, or changelog entries must be updated.

## Workflow

1. Inspect the project state before acting.
2. Break the task into checkpoints that each produce verifiable progress.
3. Choose the smallest useful unit of work for the next checkpoint.
4. Execute that checkpoint.
5. Validate immediately.
6. Record what changed, why, what failed, and what remains.
7. Continue only if the stop condition is not met and the next step is still inside scope.

## Delegation rules

Delegate only when the subtask has a clear input, output, and validation rule. Never delegate vague judgment such as "make it better" without a definition of better.

A delegated prompt must include:

```markdown
Objective:
Context:
Files/areas to inspect first:
Allowed changes:
Forbidden changes:
Validation:
Expected output:
Stop/pause condition:
```

## Autonomous loop guardrails

- Do not run an open-ended loop without a measurable stop condition.
- Do not weaken tests, delete checks, skip validations, or narrow scope just to pass.
- Do not continue after repeated failure without summarizing the blocker and changing strategy.
- Do not make unrelated refactors during an orchestrated task.

## Final output

End with:

- What changed and why
- What alternatives were rejected
- Tests run and exact output
- Risk level / blast radius
- Files touched and why each matters
- Known uncertainty
- Rollback plan
