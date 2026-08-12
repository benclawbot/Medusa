---
name: quality-gate
description: Final review gate before declaring coding work done. Use after implementation, before PRs, before merging, or when asked to review quality, risk, tests, blast radius, or release readiness.
---

# Quality Gate

Use this before declaring work complete.

## Review dimensions

1. **Correctness** — Does the implementation meet the stated objective?
2. **Scope control** — Are unrelated changes avoided?
3. **Tests** — Do tests cover meaningful behavior and pass?
4. **Build/type/lint** — Are static gates clean?
5. **Docs** — Do README/docs/examples match the change?
6. **Security** — Are secrets, unsafe file/network operations, and injection risks avoided?
7. **Compatibility** — Are APIs, data formats, migrations, and config changes safe?
8. **Rollback** — Can the change be reverted cleanly?

## Required checks

Inspect the diff before reporting. Look for:

- accidental generated files
- local machine paths
- secrets or tokens
- disabled tests
- TODOs that hide incomplete work
- dependency additions without justification
- docs that overclaim what is implemented

## Risk rating

Use:

- **Low** — isolated docs, tests, UI copy, or internal-only refactor with tests.
- **Medium** — user-visible behavior, config, dependencies, persistence, or API changes.
- **High** — auth, security, filesystem, shell, networking, migrations, billing, data loss risk, or broad architectural rewrites.

## Output format

```markdown
## Quality Gate
Status: Pass / Pass with notes / Blocked
Risk: Low / Medium / High

Checks run:
- `<command>` → <exact output or summary>

Findings:
- <issue or none>

Required fixes before merge:
- <fix or none>

Rollback:
- <revert path>
```
