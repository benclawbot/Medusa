# Codex Task Evidence Template

Use this template internally for non-trivial Medusa work. It can also be copied into a working note or PR description when useful.

```markdown
# Task

## Objective

[Exact intended outcome]

## Current behavior

[Observed behavior and evidence]

## Required behavior

[Required behavior]

## Acceptance criteria

- [ ] AC1
- [ ] AC2
- [ ] AC3

## Non-goals

- NG1
- NG2

## Constraints

- Repository/security constraints:
- Backward-compatibility constraints:
- Migration/protocol constraints:
- Platform constraints:

## Investigation

- Relevant files:
- Relevant call paths:
- Existing tests:
- Analogous implementations:
- Relevant CI/workflows:
- Unknown assumptions:

## Root cause (bug fixes)

- Observed failure:
- Immediate cause:
- Root cause:
- Why existing behavior is wrong:
- Why the proposed change addresses the root cause:

## Implementation plan

1.
2.
3.

## Validation plan

- [ ] regression test fails before fix when practical
- [ ] targeted test(s)
- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- [ ] `cargo test --workspace --all-features --locked`
- [ ] `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --locked --no-deps`
- [ ] `cargo deny check advisories sources`
- [ ] `cargo audit`
- [ ] affected platform-specific checks
- [ ] affected certification/CI checks
- [ ] final diff review
- [ ] adversarial review

## Evidence matrix

| Requirement | Implementation evidence | Validation evidence | Status |
|---|---|---|---|
| AC1 | | | NOT VERIFIED |
| AC2 | | | NOT VERIFIED |
| AC3 | | | NOT VERIFIED |

## Final status

VERIFIED / PARTIALLY VERIFIED / NOT VERIFIED / BLOCKED

## Changes

- Material behavior changed:
- Material files changed:

## Validation results

- command → PASS / FAIL / NOT RUN
- command → PASS / FAIL / NOT RUN

## Review

- Final diff inspected: YES / NO
- Security implications checked: YES / N/A / NO
- Concurrency/lifecycle implications checked: YES / N/A / NO
- Platform implications checked: YES / N/A / NO
- Compatibility/migration implications checked: YES / N/A / NO
- Material review findings resolved: YES / N/A / NO

## Remaining uncertainty

[None or explicit limitations]
```
