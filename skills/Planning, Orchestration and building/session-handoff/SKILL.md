---
name: session-handoff
description: Create a compact handoff for continuing work in a fresh agent session. Use near context limits, after long implementation sessions, when switching agents/tools, or before pausing a project.
---

# Session Handoff

A handoff should let a fresh agent continue without rediscovering state or repeating mistakes.

## Principles

- State facts, not commands.
- Capture decisions and rejected paths.
- Reference files instead of pasting large content.
- Redact secrets.
- Mark uncertainty clearly.
- Tell the next agent what to verify.

## Handoff template

```markdown
# HANDOFF: <short title>
Generated: <timestamp>

## Goal
<project objective and current task>

## Current State
DONE:
PARTIAL:
NOT STARTED:

## Key Decisions and Why
- <decision> — <reason>

## Traps / Failed Attempts
- <what was tried and why it failed>

## Relevant Files
- <path> — <why it matters>

## Validation State
- <command> → <exact result>

## Open Questions / Uncertainty
- <uncertainty>

## Suggested Next Focus
<one narrow continuation target>
```

## Rules

Do not claim the next agent can trust the handoff blindly. End with: "Verify this against the repository before acting."
