# Medusa Codex Engineering Instructions

These instructions apply to all agent-driven work in this repository.

## Objective

Optimize for correctness, evidence, minimal regression risk, and safe repository handling. Speed, confidence, and apparent completion are secondary.

Use this rule throughout the task:

> Reason about what requires inference. Inspect what can be inspected. Execute what can be tested. Verify what can be verified. Never claim more than the evidence supports.

For the full operating model, see [`docs/CODEX-RELIABILITY.md`](docs/CODEX-RELIABILITY.md).

## Evidence states

Treat material conclusions as one of:

- `VERIFIED` — directly supported by repository inspection, executed commands, test output, CI, or authoritative documentation.
- `INFERRED` — strongly supported but not directly verified.
- `UNKNOWN` — insufficient evidence.

Never silently convert `INFERRED` or `UNKNOWN` into `VERIFIED`.

Do not claim that code compiles, tests pass, CI is green, a PR is merged, an issue is closed, a platform is supported, or an external API behaves a certain way unless that state was directly observed.

## Required workflow

For every non-trivial change:

1. Read the complete task and derive explicit acceptance criteria.
2. Inspect repository instructions and the relevant implementation.
3. Inspect direct callers, callees, tests, analogous code, platform-specific behavior, and compatibility constraints.
4. Identify assumptions and verify them when practical.
5. For bugs, reproduce the failure before repairing it when practical.
6. Make the smallest coherent change that fully satisfies the task.
7. Add or update behavioral tests, including a regression test for bug fixes when practical.
8. Run targeted validation first.
9. Run every applicable required repository check.
10. Inspect the final diff for accidental or unrelated changes.
11. Adversarially review the implementation against the original requirements.
12. Map every acceptance criterion to implementation and validation evidence.
13. Report exact results and remaining uncertainty.

Do not begin by guessing a solution and then searching only for evidence that supports it.

## Scope discipline

Prefer the smallest correct diff.

Do not:

- perform unrelated refactors;
- introduce speculative abstractions;
- rename unrelated symbols;
- change public behavior not required by the task;
- upgrade unrelated dependencies;
- broaden the task silently;
- delete functionality merely because it appears unused;
- hide failures behind fallback behavior;
- weaken tests to make an implementation pass.

Preserve existing architecture and conventions unless evidence justifies changing them.

## Repository-specific invariants

Preserve Medusa's containment, sandbox, rollback, credential-redaction, migration, and durability guarantees.

For serialized session, protocol, or configuration changes, preserve backward compatibility unless a migration is included. Follow [`docs/PROTOCOL-VERSIONING.md`](docs/PROTOCOL-VERSIONING.md) for protocol and configuration changes.

Never commit provider credentials, generated `.medusa` state, build outputs, or local test artifacts.

When user-visible commands, configuration, behavior, or compatibility changes, update the relevant documentation.

## Bug-fix contract

For bug fixes, use this sequence whenever practical:

1. Observe the failure.
2. Identify the execution path.
3. Determine the root cause.
4. Add or identify a regression test.
5. Confirm the regression test fails for the expected reason before the fix.
6. Implement the fix.
7. Confirm the regression test passes.
8. Run surrounding tests.
9. Run broader required validation.

A bug fix is not complete merely because the symptom disappears. Understand and address the root cause.

## Testing

Tests should prove behavior, not implementation details.

Consider nominal, boundary, invalid-input, failure, cleanup, concurrency, compatibility, security, and platform cases as applicable.

When a validation run reports multiple failures:

1. collect all available failures;
2. classify them;
3. identify shared or cascading causes;
4. fix root causes;
5. rerun targeted checks;
6. rerun broader validation.

Do not repeatedly patch only the first visible failure when more information is available.

Treat flaky tests as evidence. Do not rerun until green and call the result verified without investigating the instability.

## Required validation

Run the same core checks required by the repository before opening a pull request when applicable:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --locked --no-deps
cargo deny check advisories sources
cargo audit
```

Also run task-specific and platform-specific checks required by the affected area. The release workflows may enforce additional coverage, adversarial containment and rollback regressions, fuzz/migration/chaos scenarios, package smoke tests, browser certification, and credential-gated tests.

Never describe the entire validation suite as passing when only a subset was executed.

## Security review

For trust-boundary changes, explicitly inspect applicable risks including:

- input validation;
- path traversal;
- command execution and quoting;
- environment-variable handling;
- secrets and credential redaction;
- filesystem and repository boundaries;
- network boundaries;
- authentication and authorization;
- sandboxing and process containment;
- privilege boundaries;
- unsafe code and FFI boundaries;
- deserialization and injection;
- temporary files and cleanup;
- denial-of-service risk.

Security assumptions require evidence when practical.

## Concurrency and lifecycle review

For concurrent, asynchronous, process, or resource-owning code, inspect:

- races and deadlocks;
- ordering and atomicity;
- cancellation and timeout behavior;
- retries and idempotency;
- partial failure;
- process/task-tree termination;
- lock and handle ownership;
- cleanup on success and failure;
- resource leaks.

Do not treat successful normal execution as sufficient proof for concurrent code.

## Platform review

Do not infer equivalent behavior across Linux, macOS, and Windows when platform semantics matter.

Check path, permission, process, signal, environment, filesystem, networking, packaging, and containment differences as applicable. Platform-specific claims require platform-specific evidence when consequential.

## Git safety

Before consequential Git operations, inspect repository state.

Never:

- discard or overwrite unrelated user changes;
- reset unrelated files;
- force-push without explicit authorization;
- rewrite history unnecessarily;
- delete branches unexpectedly;
- claim a commit, push, PR, merge, CI result, or issue closure that was not observed.

Keep task changes isolated and attributable to the requested work.

## Pull requests and CI

Before opening or updating a PR:

- inspect the final diff;
- run applicable validation;
- describe the problem and intended behavior;
- describe the implementation approach;
- list exact tests and evidence;
- disclose security, migration, rollback, or platform impact;
- disclose anything not verified.

When CI fails, inspect all available failing jobs and errors before changing code. Distinguish task-related failures from unrelated infrastructure failures.

All required checks must pass before merge.

## Issue workflow

When asked to handle repository issues one at a time:

1. verify the issue is open;
2. verify whether an existing PR already addresses it when relevant;
3. derive acceptance criteria;
4. create an isolated branch/worktree when appropriate;
5. reproduce the issue;
6. implement the smallest correct fix;
7. add regression evidence;
8. run required validation;
9. review the final diff;
10. open/update the PR;
11. inspect all CI failures;
12. fix task-related failures and rerun validation;
13. merge only when authorized and all gates pass;
14. verify the merge;
15. verify related issue closure rather than assuming it.

## Adversarial review

Before completion, switch from author mindset to reviewer mindset and attempt to disprove the implementation.

Ask:

- Which requirement could still be unmet?
- What assumption was made without evidence?
- What boundary input breaks this?
- What happens on failure or cancellation?
- What happens during cleanup?
- What happens concurrently?
- What happens on another supported platform?
- Could the tests pass while required behavior is still wrong?
- Did the change alter public or serialized behavior unexpectedly?
- Did the diff include unrelated changes?

Resolve material findings before declaring completion.

## Completion gate

A task is `VERIFIED` only when all applicable conditions are satisfied:

```text
requirements mapped
AND implementation complete
AND targeted tests pass
AND broader relevant tests pass
AND formatter passes
AND linter/static analysis passes
AND type/compiler/documentation checks pass
AND security/policy checks pass
AND platform checks pass where required
AND final diff reviewed
AND no unexplained changes remain
AND regression behavior is proven when applicable
AND review findings are resolved
AND remaining uncertainty is disclosed
```

If a required element is missing, report `PARTIALLY VERIFIED`, `NOT VERIFIED`, or `BLOCKED` instead of inferring success.

## Final report

Use this structure for non-trivial coding work:

```text
STATUS
VERIFIED / PARTIALLY VERIFIED / NOT VERIFIED / BLOCKED

CHANGES
- Material behavior changed
- Material files changed

REQUIREMENTS
- AC1: VERIFIED
- AC2: VERIFIED
- AC3: NOT VERIFIED

VALIDATION
- command -> PASS
- command -> PASS
- command -> FAIL / NOT RUN

REVIEW
- Final diff inspected
- Relevant risks checked
- Findings resolved

REMAINING UNCERTAINTY
- None
or
- Explicit unresolved limitation
```

Never hide skipped checks.
