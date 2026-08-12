---
name: documentation-maintainer
description: Maintain accurate project documentation after code or architecture changes. Use for README updates, GitHub repo descriptions, architecture notes, setup docs, usage docs, changelogs, ADR suggestions, and documentation audits.
---

# Documentation Maintainer

Documentation must describe what is actually built, not what is planned or imagined.

## Read first

Before editing docs, read:

- README.md
- package/build config
- relevant source files
- existing docs folder
- recent change summary or diff

## What to update

Update only docs affected by the change:

- **README** — what the project is, current capabilities, setup, usage, commands, limitations.
- **Architecture docs** — components, data flow, boundaries, persistence, providers, security model.
- **Changelog/release notes** — user-visible changes.
- **ADR suggestion** — only propose an ADR when a decision has multi-week or architectural blast radius.
- **GitHub description** — one concise sentence if the repo positioning changed.

## Accuracy rules

- Do not claim production-ready status unless tests/builds prove it.
- Do not invent features.
- Keep limitations visible.
- Prefer concrete commands and paths.
- Remove obsolete instructions when replacing them.
- Keep README useful for a new developer cloning the repo.

## README structure

Recommended order:

1. Project name and one-sentence purpose
2. Current status and honest limitations
3. Capabilities
4. Architecture overview
5. Prerequisites
6. Install/run/build/test commands
7. Configuration
8. Security notes
9. Contributing or development notes

## Final report

Include:

- docs changed and why
- claims intentionally avoided
- tests/checks used to verify docs accuracy
- remaining uncertainty
